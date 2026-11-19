//! Name resolution and type checking.
//!
//! Passes:
//!   1. `collect`: register every item's name (structs, enums, traits, functions, ...)
//!      and process `use` imports.
//!   2. `lower`: turn syntactic types into semantic `Ty`s; build signatures for
//!      functions, impls and traits; check that impls match their traits.
//!   3. bodies: infer the type of every expression, resolve names and methods,
//!      check call-site `mut` markers and assignment mutability.
//!
//! Results are side tables keyed by the address of the AST node (`NodeKey`), so they
//! are only valid for the exact `Module` they were computed from.

mod collect;
mod expr;
mod lower;
mod pat;
mod traits;

use std::collections::HashMap;

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::infer::{Infer, VarKind};
use crate::prelude::{self, ModuleSrc};
use crate::unit::ModRef;
use crate::program::*;
use crate::span::Span;
use crate::types::*;
// `Dim` exists in both `ast` and `types`; inside the checker it means the semantic one.
use crate::types::Dim;

// ------------------------------------------------------------------ public results

/// A fast hasher for `NodeKey`s: they are addresses, so there is nothing to defend
/// against, and the interpreter looks them up for every expression it evaluates.
#[derive(Default, Clone, Copy)]
pub struct PtrHasher(u64);

impl std::hash::Hasher for PtrHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ *b as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn write_usize(&mut self, n: usize) {
        let k = ((n as u64) >> 3).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        self.0 = k ^ (k >> 32);
    }
}

/// A map keyed by AST node.
pub type NodeMap<V> = HashMap<NodeKey, V, std::hash::BuildHasherDefault<PtrHasher>>;

/// Identifies an AST node by address.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeKey(pub usize);

impl NodeKey {
    pub fn of<T>(r: &T) -> NodeKey {
        NodeKey(r as *const T as usize)
    }
}

/// What a name or operator expression refers to.
#[derive(Clone, Debug)]
pub enum Res {
    Local(u32),
    /// A function, method or overloaded operator, with the types its generic
    /// parameters were instantiated to (the impl's/trait's first, then its own).
    Fn(FnId, Vec<(GenericId, Ty)>),
    /// A construct the runtime implements directly (tensor constructors).
    Builtin(&'static str),
    Const(ConstId),
    StructCtor(AdtId, Vec<Ty>),
    Variant(AdtId, usize, Vec<Ty>),
}

#[derive(Clone, Debug)]
pub struct Binding {
    pub name: String,
    pub ty: Ty,
    pub span: Span,
    pub mutable: bool,
}

#[derive(Default, Debug)]
pub struct Results {
    pub expr_ty: NodeMap<Ty>,
    pub expr_res: NodeMap<Res>,
    /// The type of every pattern (and so of every binding it introduces).
    pub pat_ty: NodeMap<Ty>,
    /// Which function each `fn` declaration became.
    pub fn_decl: NodeMap<FnId>,
    /// Which constant each `const` declaration became.
    pub const_decl: NodeMap<ConstId>,
    /// Every binding (parameters, `let`s, pattern variables) in the user's module.
    pub bindings: Vec<Binding>,
}

pub struct CheckResult {
    pub program: Program,
    pub results: Results,
    pub diagnostics: Vec<Diagnostic>,
}

impl CheckResult {
    pub fn show(&self, t: &Ty) -> String {
        self.program.show(t, &|_| "_".to_string())
    }

    /// The (first) binding named `name`, rendered as a type string.
    pub fn binding_ty(&self, name: &str) -> Option<String> {
        self.results
            .bindings
            .iter()
            .find(|b| b.name == name)
            .map(|b| self.show(&b.ty))
    }

    pub fn expr_ty(&self, e: &Expr) -> Option<String> {
        self.results
            .expr_ty
            .get(&NodeKey::of(e))
            .map(|t| self.show(t))
    }
}

/// Is this tensor builtin a method of a tensor value (`t.sum()`), rather than a constructor (`Type.zeros(2)`)?
pub fn tensor_builtin_has_receiver(kind: &str) -> bool {
    matches!(kind, "tensor.sum" | "tensor.mean" | "tensor.min" | "tensor.max" | "tensor.shape" | "tensor.to_vec" | "tensor.map" | "tensor.reshape" | "tensor.transpose")
}

/// Parse `src` and check it against the built-in library.
pub fn check_source(src: &str) -> Result<CheckResult, Diagnostic> {
    let user = crate::parser::parse_module(src)?;
    let lib = prelude::load();
    Ok(check(&lib, &user))
}

/// Type-check `user` against the library modules `lib` (the module is called `main`).
pub fn check(lib: &[ModuleSrc], user: &Module) -> CheckResult {
    let mut mods: Vec<ModRef> = lib.iter().map(|m| m.as_ref()).collect();
    mods.push(ModRef { path: "main", module: user, builtin: false, package: "" });
    let root = mods.len() - 1;
    check_modules(&mods, root)
}

/// Type-check a whole program: every module (library modules first, `prelude` the very first),
/// and the index of the entry module, which holds `main`.
pub fn check_modules(mods: &[ModRef], root: usize) -> CheckResult {
    let mut c = Checker::new(mods, root);
    c.run();
    CheckResult {
        program: c.prog,
        results: c.results,
        diagnostics: c.diags,
    }
}

// ------------------------------------------------------------------ internals

#[derive(Clone, Copy, Debug)]
pub(crate) enum TypeItem {
    Adt(AdtId),
    Trait(TraitId),
    Alias(usize),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ValueItem {
    Fn(FnId),
    Const(ConstId),
    StructCtor(AdtId),
    Variant(AdtId, usize),
}

#[derive(Default)]
pub(crate) struct ModScope {
    pub types: HashMap<String, TypeItem>,
    pub values: HashMap<String, ValueItem>,
    /// Imported modules by the name they are known as here.
    pub modules: HashMap<String, usize>,
}

#[derive(Clone, Copy)]
pub(crate) enum AdtSrc<'a> {
    Struct(&'a StructDecl),
    Enum(&'a EnumDecl),
}

pub(crate) struct AliasEntry<'a> {
    pub decl: &'a TypeAliasDecl,
    pub module: usize,
    pub public: bool,
    pub generics: Vec<(String, GenericId)>,
    pub ty: Option<Ty>,
    pub resolving: bool,
}

/// Everything needed to lower the types inside one item.
#[derive(Clone, Default)]
pub(crate) struct LowerCx {
    pub module: usize,
    pub generics: Vec<(String, GenericId)>,
    pub self_ty: Option<Ty>,
    pub allow_infer: bool,
}

pub(crate) struct Local {
    pub name: String,
    pub ty: Ty,
    pub mutable: bool,
}

pub(crate) struct Obligation {
    pub ty: Ty,
    pub trait_: TraitId,
    pub args: Vec<Ty>,
    pub span: Span,
}

#[derive(Default)]
pub(crate) struct LoopCtx {
    /// Type of values given to `break value` (only meaningful for `loop`).
    pub break_ty: Option<Ty>,
    pub is_loop: bool,
}

/// State for the function body currently being checked.
#[derive(Default)]
pub(crate) struct Body {
    pub cx: LowerCx,
    pub ret: Vec<Ty>,
    pub locals: Vec<Local>,
    pub visible: Vec<usize>,
    pub marks: Vec<usize>,
    pub loops: Vec<LoopCtx>,
    pub unsafe_depth: u32,
    pub obligations: Vec<Obligation>,
    pub touched: Vec<NodeKey>,
    pub touched_pats: Vec<NodeKey>,
    /// The type of every interpolation that has a format spec, checked against it once types are known.
    pub fmt_checks: Vec<(Ty, crate::fmtspec::Fmt)>,
    /// Integer literals (node, value, span, directly negated?), checked against their final
    /// type once inference is done.
    pub int_lits: Vec<(NodeKey, u128, Span, bool)>,
    pub local_fns: Vec<HashMap<String, ValueItem>>,
    /// Index into `results.bindings` where this body's bindings start.
    pub bind_start: usize,
}

pub(crate) struct Checker<'a> {
    pub mods: Vec<(String, &'a Module)>,
    /// Library modules (functions may lack a body; no hovers, no bindings).
    pub mod_builtin: Vec<bool>,
    /// Each module's package prefix, for looking up its `use` paths.
    pub mod_package: Vec<String>,
    pub prog: Program,
    pub infer: Infer,
    pub scopes: Vec<ModScope>,
    pub mod_by_path: HashMap<String, usize>,
    pub diags: Vec<Diagnostic>,
    pub results: Results,

    pub fn_decls: Vec<Option<&'a FnDecl>>,
    pub fn_cx: Vec<LowerCx>,
    pub adt_srcs: Vec<AdtSrc<'a>>,
    pub trait_srcs: Vec<&'a TraitDecl>,
    pub trait_attrs: Vec<&'a [Attribute]>,
    pub impl_srcs: Vec<(&'a ImplDecl, usize)>,
    pub const_srcs: Vec<Option<&'a ConstDecl>>,
    pub aliases: Vec<AliasEntry<'a>>,
    /// Functions found nested inside bodies, queued for checking.
    pub queued: Vec<FnId>,

    pub body: Body,
}

impl<'a> Checker<'a> {
    fn new(refs: &[ModRef<'a>], root: usize) -> Checker<'a> {
        let mods: Vec<(String, &'a Module)> = refs.iter().map(|m| (m.path.to_string(), m.module)).collect();
        let mod_builtin = refs.iter().map(|m| m.builtin).collect();
        let mod_package = refs.iter().map(|m| m.package.to_string()).collect();
        let _ = root; // the entry module is found by the later stages (it holds `main`)
        // (the entry module is called `main`, but no module can import it by that name)
        let mod_by_path = mods
            .iter()
            .enumerate()
            .filter(|(_, (p, _))| p != "main")
            .map(|(i, (p, _))| (p.clone(), i))
            .collect();
        let scopes = mods.iter().map(|_| ModScope::default()).collect();
        Checker {
            mods,
            mod_builtin,
            mod_package,
            prog: Program::default(),
            infer: Infer::default(),
            scopes,
            mod_by_path,
            diags: Vec::new(),
            results: Results::default(),
            fn_decls: Vec::new(),
            fn_cx: Vec::new(),
            adt_srcs: Vec::new(),
            trait_srcs: Vec::new(),
            trait_attrs: Vec::new(),
            impl_srcs: Vec::new(),
            const_srcs: Vec::new(),
            aliases: Vec::new(),
            queued: Vec::new(),
            body: Body::default(),
        }
    }

    fn run(&mut self) {
        self.collect_items();
        self.resolve_imports();
        self.lower_signatures();
        self.check_bodies();
    }

    // ------------------------------------------------------------ diagnostics

    pub fn err(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic::new(msg, span));
    }

    pub fn err_hint(&mut self, span: Span, msg: impl Into<String>, hint: impl Into<String>) {
        self.diags.push(Diagnostic::new(msg, span).with_hint(hint));
    }

    /// Render a type, resolving inference variables as far as known.
    pub fn show(&self, t: &Ty) -> String {
        let r = self.infer.resolve(t);
        self.prog.show(&r, &|v| match self.infer.kind_of(v) {
            VarKind::General => "_".to_string(),
            VarKind::Int => "{integer}".to_string(),
            VarKind::Float => "{float}".to_string(),
        })
    }

    pub fn fresh(&mut self, span: Span) -> Ty {
        self.infer.fresh(VarKind::General, span)
    }

    /// Is this module part of the program (not the compiler's library)?
    pub fn is_user(&self, module: usize) -> bool {
        !self.mod_builtin[module]
    }

    // ------------------------------------------------------------ locals

    pub fn push_scope(&mut self) {
        self.body.marks.push(self.body.visible.len());
    }

    pub fn pop_scope(&mut self) {
        let m = self.body.marks.pop().unwrap_or(0);
        self.body.visible.truncate(m);
    }

    pub fn declare_local(&mut self, name: &str, ty: Ty, mutable: bool, span: Span) -> u32 {
        self.body.locals.push(Local {
            name: name.to_string(),
            ty: ty.clone(),
            mutable,
        });
        let id = self.body.locals.len() - 1;
        self.body.visible.push(id);
        if self.is_user(self.body.cx.module) && name != "_" {
            self.results.bindings.push(Binding {
                name: name.to_string(),
                ty,
                span,
                mutable,
            });
        }
        id as u32
    }

    pub fn lookup_local(&self, name: &str) -> Option<u32> {
        self.body
            .visible
            .iter()
            .rev()
            .find(|&&i| self.body.locals[i].name == name)
            .map(|&i| i as u32)
    }

    // ------------------------------------------------------------ recording

    pub fn record(&mut self, e: &Expr, ty: &Ty) {
        let k = NodeKey::of(e);
        self.results.expr_ty.insert(k, ty.clone());
        self.body.touched.push(k);
    }

    pub fn record_res(&mut self, e: &Expr, res: Res) {
        self.results.expr_res.insert(NodeKey::of(e), res);
    }

    // ------------------------------------------------------------ bodies

    /// `#[test]` functions are run with no arguments; they return nothing, or a `Result`.
    fn check_test_functions(&mut self) {
        let modules: Vec<&'a Module> = (0..self.mods.len()).filter(|&m| self.is_user(m)).map(|m| self.mods[m].1).collect();
        for module in modules {
          for item in &module.items {
            let ItemKind::Fn(f) = &item.kind else { continue };
            if !item.attrs.iter().any(|a| a.name.name == "test") {
                continue;
            }
            if !f.params.is_empty() || !f.generics.is_empty() {
                self.err_hint(
                    f.name.span,
                    format!("the test function `{}` cannot take parameters or be generic", f.name.name),
                    "tests are called with no arguments",
                );
                continue;
            }
            let Some(id) = self.results.fn_decl.get(&NodeKey::of(f)).copied() else { continue };
            let ret = self.prog.fns[id.idx()].sig.ret.clone();
            let is_result = matches!(&ret, Ty::Adt(a, _) if Some(*a) == self.prog.find_adt("Result"));
            if !ret.is_unit() && !is_result {
                let shown = self.show(&ret);
                self.err_hint(
                    f.name.span,
                    format!("the test function `{}` returns `{shown}`", f.name.name),
                    "a test returns nothing, or a `Result` that fails the test when it is `Err`",
                );
            }
          }
        }
    }

    fn check_bodies(&mut self) {
        self.check_test_functions();
        // free functions, methods and trait defaults of the program's modules (and library functions that have a body)
        let ids: Vec<FnId> = (0..self.prog.fns.len())
            .filter(|&i| match self.fn_decls[i] {
                Some(d) => self.is_user(self.prog.fns[i].module) || d.body.is_some(),
                None => false,
            })
            .map(|i| FnId(i as u32))
            .collect();
        for id in ids {
            self.check_fn(id);
        }
        // functions nested inside bodies are queued while checking
        while let Some(id) = self.queued.pop() {
            self.check_fn(id);
        }
        // constants
        let consts: Vec<usize> = (0..self.prog.consts.len())
            .filter(|&i| self.is_user(self.prog.consts[i].module) && self.const_srcs[i].is_some())
            .collect();
        for i in consts {
            self.check_const(ConstId(i as u32));
        }
    }

    fn begin_body(&mut self, cx: LowerCx) {
        self.body = Body {
            cx: LowerCx {
                allow_infer: true,
                ..cx
            },
            bind_start: self.results.bindings.len(),
            ..Body::default()
        };
        self.push_scope();
    }

    fn check_const(&mut self, id: ConstId) {
        let decl = self.const_srcs[id.idx()].unwrap();
        let module = self.prog.consts[id.idx()].module;
        let ty = self.prog.consts[id.idx()].ty.clone();
        self.begin_body(LowerCx {
            module,
            ..LowerCx::default()
        });
        self.expr_coerce(&decl.value, &ty);
        self.finish_body();
    }

    fn check_fn(&mut self, id: FnId) {
        let Some(decl) = self.fn_decls[id.idx()] else { return };
        let Some(body) = &decl.body else {
            let in_trait = matches!(self.prog.fns[id.idx()].parent, Parent::Trait(_));
            if self.is_user(self.prog.fns[id.idx()].module) && !in_trait {
                self.err_hint(
                    decl.name.span,
                    format!("function `{}` has no body", decl.name.name),
                    "give it a body `{ ... }`",
                );
            }
            return;
        };
        let def = self.prog.fns[id.idx()].clone();
        let cx = self.fn_cx[id.idx()].clone();
        self.begin_body(cx.clone());
        self.body.ret.push(def.sig.ret.clone());
        if def.is_unsafe {
            self.body.unsafe_depth += 1;
        }
        if let Some(mode) = def.sig.self_mode {
            let self_ty = cx.self_ty.clone().unwrap_or(Ty::Error);
            let span = decl.params.first().map(|p| p.span).unwrap_or(decl.name.span);
            self.declare_local("self", self_ty, mode != ParamMode::Borrow, span);
        }
        for (p, pd) in decl
            .params
            .iter()
            .filter(|p| p.name.name != "self")
            .zip(&def.sig.params)
        {
            self.declare_local(&pd.name, pd.ty.clone(), pd.mode != ParamMode::Borrow, p.span);
        }
        let ty = self.block(body, Some(&def.sig.ret));
        let span = match body.stmts.last() {
            Some(Stmt::Expr(e)) => e.span,
            _ => Span::new(body.span.end.saturating_sub(1), body.span.end),
        };
        // Without semicolons a function cannot discard a value with `f();`, so a
        // function that returns unit may end in an expression of any type.
        if !def.sig.ret.is_unit() {
            self.coerce(&ty, &def.sig.ret, span);
        }
        self.finish_body();
    }

    /// Default literal types, check deferred trait obligations, and write resolved
    /// types back into the results.
    fn finish_body(&mut self) {
        self.infer.apply_defaults();
        let fmt_checks = std::mem::take(&mut self.body.fmt_checks);
        for (ty, fmt) in fmt_checks {
            let ty = self.infer.resolve(&ty);
            let kind = match &ty {
                Ty::Int(_) => crate::fmtspec::Kind::Int,
                Ty::Float(_) => crate::fmtspec::Kind::Float,
                Ty::Complex(_) => crate::fmtspec::Kind::Complex,
                Ty::Str => crate::fmtspec::Kind::Text,
                Ty::Char | Ty::Bool => crate::fmtspec::Kind::Word,
                Ty::Adt(a, _) if self.prog.adt(*a).name == "String" => crate::fmtspec::Kind::Text,
                Ty::Var(_) | Ty::Error => continue,
                _ => crate::fmtspec::Kind::Other,
            };
            if let Err(why) = crate::fmtspec::check(kind, &fmt.spec) {
                self.err_hint(fmt.span, format!("this format does not fit the value: {why}"), "see the format list in SPEC.md, section 8d");
            }
        }
        let obligations = std::mem::take(&mut self.body.obligations);
        for ob in obligations {
            let ty = self.infer.resolve(&ob.ty);
            if matches!(ty, Ty::Var(_) | Ty::Error) {
                continue;
            }
            let args: Vec<Ty> = ob.args.iter().map(|a| self.infer.resolve(a)).collect();
            if !self.implements(&ty, ob.trait_, &args) {
                let tname = self.prog.trait_(ob.trait_).name.clone();
                let shown = self.show(&ty);
                let hint = if matches!(ty, Ty::Param(_)) {
                    format!("add the bound `{shown}: {tname}` to the generic parameter")
                } else {
                    format!("add `impl {tname} for {shown}` or use a type that has one")
                };
                self.err_hint(
                    ob.span,
                    format!("the trait `{tname}` is not implemented for `{shown}`"),
                    hint,
                );
            }
        }
        // write back: resolve every type recorded for this body. Variables that are
        // still unbound are reported below and become the error type, so none outlives
        // the inference table.
        let touched = std::mem::take(&mut self.body.touched);
        for k in touched {
            if let Some(t) = self.results.expr_ty.get(&k).cloned() {
                let r = self.infer.zonk(&t);
                self.results.expr_ty.insert(k, r);
            }
            if let Some(Res::Fn(f, args)) = self.results.expr_res.get(&k).cloned() {
                let args = args.iter().map(|(g, a)| (*g, self.infer.zonk(a))).collect();
                self.results.expr_res.insert(k, Res::Fn(f, args));
            }
            if let Some(Res::StructCtor(a, args)) = self.results.expr_res.get(&k).cloned() {
                let args = args.iter().map(|x| self.infer.zonk(x)).collect();
                self.results.expr_res.insert(k, Res::StructCtor(a, args));
            }
            if let Some(Res::Variant(a, i, args)) = self.results.expr_res.get(&k).cloned() {
                let args = args.iter().map(|x| self.infer.zonk(x)).collect();
                self.results.expr_res.insert(k, Res::Variant(a, i, args));
            }
        }
        let lits = std::mem::take(&mut self.body.int_lits);
        if self.is_user(self.body.cx.module) {
            for (k, value, span, negated) in lits {
                let Some(Ty::Int(it)) = self.results.expr_ty.get(&k).cloned() else { continue };
                let (bits, signed) = int_shape(it);
                let max: u128 = if signed { (1u128 << (bits - 1)) - 1 } else if bits == 128 { u128::MAX } else { (1u128 << bits) - 1 };
                let limit = if negated && signed { max + 1 } else { max };
                if value > limit {
                    let range = if signed {
                        format!("-{} to {}", max + 1, max)
                    } else {
                        format!("0 to {max}")
                    };
                    self.err_hint(span, format!("the literal `{value}` does not fit in `{}`", it.name()), format!("`{}` holds {range}", it.name()));
                }
            }
        }
        let pats = std::mem::take(&mut self.body.touched_pats);
        for k in pats {
            if let Some(t) = self.results.pat_ty.get(&k).cloned() {
                let r = self.infer.zonk(&t);
                self.results.pat_ty.insert(k, r);
            }
        }
        for i in self.body.bind_start..self.results.bindings.len() {
            let t = self.results.bindings[i].ty.clone();
            self.results.bindings[i].ty = self.infer.zonk(&t);
        }
        let mut reported: Vec<Span> = Vec::new();
        // anything still unresolved is an error
        for i in 0..self.infer.vars.len() {
            let v = &self.infer.vars[i];
            if v.binding.is_none() && !reported.contains(&v.span) {
                let span = v.span;
                reported.push(span);
                if self.is_user(self.body.cx.module) {
                    self.err_hint(
                        span,
                        "cannot infer a type here",
                        "add a type annotation, e.g. `let x: Vec<f64> = ...`",
                    );
                }
            }
        }
        self.infer = Infer::default();
        self.body = Body::default();
    }
}

/// Width in bits and signedness of an integer type.
fn int_shape(it: IntTy) -> (u32, bool) {
    use IntTy::*;
    match it {
        I8 => (8, true),
        I16 => (16, true),
        I32 => (32, true),
        I64 | Isize => (64, true),
        I128 => (128, true),
        U8 => (8, false),
        U16 => (16, false),
        U32 => (32, false),
        U64 | Usize => (64, false),
        U128 => (128, false),
    }
}
