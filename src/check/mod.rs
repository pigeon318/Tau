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
use crate::program::*;
use crate::span::Span;
use crate::types::*;
// `Dim` exists in both `ast` and `types`; inside the checker it means the semantic one.
use crate::types::Dim;

// ------------------------------------------------------------------ public results

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
    /// A function, method or overloaded operator, with its generic arguments
    /// (the impl's/trait's generics first, then the function's own).
    Fn(FnId, Vec<Ty>),
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
    pub expr_ty: HashMap<NodeKey, Ty>,
    pub expr_res: HashMap<NodeKey, Res>,
    /// The type of every pattern (and so of every binding it introduces).
    pub pat_ty: HashMap<NodeKey, Ty>,
    /// Which function each `fn` declaration became.
    pub fn_decl: HashMap<NodeKey, FnId>,
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

/// Parse `src` and check it against the built-in library.
pub fn check_source(src: &str) -> Result<CheckResult, Diagnostic> {
    let user = crate::parser::parse_module(src)?;
    let lib = prelude::load();
    Ok(check(&lib, &user))
}

pub fn check(lib: &[ModuleSrc], user: &Module) -> CheckResult {
    let mut c = Checker::new(lib, user);
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
    pub local_fns: Vec<HashMap<String, ValueItem>>,
    /// Index into `results.bindings` where this body's bindings start.
    pub bind_start: usize,
}

pub(crate) struct Checker<'a> {
    pub mods: Vec<(String, &'a Module)>,
    pub user: usize,
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
    fn new(lib: &'a [ModuleSrc], user: &'a Module) -> Checker<'a> {
        let mut mods: Vec<(String, &'a Module)> =
            lib.iter().map(|m| (m.path.clone(), &m.module)).collect();
        mods.push(("main".to_string(), user));
        let user_idx = mods.len() - 1;
        let mod_by_path = mods
            .iter()
            .enumerate()
            .map(|(i, (p, _))| (p.clone(), i))
            .collect();
        let scopes = mods.iter().map(|_| ModScope::default()).collect();
        Checker {
            mods,
            user: user_idx,
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

    pub fn is_user(&self, module: usize) -> bool {
        module == self.user
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

    fn check_bodies(&mut self) {
        // free functions, methods and trait defaults defined in the user's module
        let user = self.user;
        let ids: Vec<FnId> = (0..self.prog.fns.len())
            .filter(|&i| self.prog.fns[i].module == user && self.fn_decls[i].is_some())
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
            .filter(|&i| self.prog.consts[i].module == user && self.const_srcs[i].is_some())
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
                let args = args.iter().map(|a| self.infer.zonk(a)).collect();
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
                if self.body.cx.module == self.user {
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
