//! C code generation from the checked AST.
//!
//! Tau is expression-oriented and C is not, so expressions are emitted as GNU statement
//! expressions (`({ ...; value; })`, supported by clang and gcc). Control flow uses `goto`
//! rather than C `break`/`continue`, so a Tau `break` is never captured by a construct the
//! generator itself introduced. Evaluation order is made explicit with temporaries, since C
//! leaves the order of operands and arguments open.
//!
//! Generic functions are monomorphized: every call records the concrete types of the
//! callee's generic parameters, and each distinct instantiation becomes one C function.
//!
//! Anything outside the supported subset is reported as "unsupported", and the caller falls
//! back to the interpreter.

use std::collections::{HashMap, HashSet};

use super::NativeError;
use crate::ast::*;
use crate::borrowck::CaptureKind;
use crate::check::{CheckResult, NodeKey, Res, Results};
use crate::interp::match_ty;
use crate::program::*;
use crate::types::*;

type CgResult<T> = Result<T, NativeError>;

fn unsupported<T>(what: impl Into<String>) -> CgResult<T> {
    Err(NativeError::unsupported(format!("{} is not supported by the native backend yet", what.into())))
}

type Subst = Vec<(GenericId, Ty)>;

#[derive(Clone)]
struct Local {
    name: String,
    cname: String,
    ty: Ty,
    /// A `mut` parameter: the C variable is a pointer to the caller's value.
    by_ptr: bool,
    /// This local owns heap memory and is dropped when its scope ends (unless moved out,
    /// which zeroes it, so dropping is always safe).
    owns: bool,
}

struct LoopCtx {
    brk: String,
    cont: String,
    /// `scopes.len()` when the loop started: `break`/`continue` drop everything above it.
    depth: usize,
    /// The C variable holding the value of a `loop ... break value`.
    result: Option<String>,
}

/// One monomorphic instance of a function waiting to be generated.
struct Inst {
    name: String,
    fid: FnId,
    sub: Subst,
}

/// What a call resolved to.
enum Target {
    User { name: String, fid: FnId, sub: Subst },
    Builtin { key: String, fid: FnId, sub: Subst },
}

pub struct Cg<'a> {
    prog: &'a Program,
    res: &'a Results,
    decls: HashMap<FnId, &'a FnDecl>,
    consts: HashMap<ConstId, &'a ConstDecl>,
    types: String,
    protos: String,
    bodies: String,
    type_names: HashMap<String, String>,
    helpers: HashSet<String>,
    queue: Vec<Inst>,
    instances: HashMap<String, String>,
    seen_consts: HashSet<ConstId>,
    counter: usize,
    scopes: Vec<Vec<Local>>,
    loops: Vec<LoopCtx>,
    cur_ret: Ty,
    /// The generic arguments of the instance being generated.
    sub: Subst,
    /// Declarations of temporaries hoisted to the top of the current function.
    hoisted: String,
    /// Temporaries (name, type) created while compiling the current statement; dropped when
    /// it ends.
    temps: Vec<Vec<(String, Ty)>>,
    /// Dotted path of each library module (`"prelude"`, `"std.fs"`, ...), by module index.
    mod_paths: Vec<String>,
    /// Set while compiling the target of an assignment: `m[k] = v` and `m[k] += v` create a
    /// missing map entry, any other use of `m[k]` panics if it is missing.
    insert_ctx: bool,
    /// How each closure captures the variables it uses (from the borrow checker).
    captures: HashMap<NodeKey, Vec<(String, CaptureKind)>>,
}

/// Translate a checked program to a complete C source file (runtime included).
pub fn translate(user: &Module, check: &CheckResult, mod_paths: Vec<String>, captures: HashMap<NodeKey, Vec<(String, CaptureKind)>>) -> Result<String, NativeError> {
    let mut cg = Cg {
        prog: &check.program,
        res: &check.results,
        decls: HashMap::new(),
        consts: HashMap::new(),
        types: String::new(),
        protos: String::new(),
        bodies: String::new(),
        type_names: HashMap::new(),
        helpers: HashSet::new(),
        queue: Vec::new(),
        instances: HashMap::new(),
        seen_consts: HashSet::new(),
        counter: 0,
        scopes: Vec::new(),
        loops: Vec::new(),
        cur_ret: Ty::unit(),
        sub: Vec::new(),
        hoisted: String::new(),
        temps: Vec::new(),
        mod_paths,
        insert_ctx: false,
        captures,
    };
    for item in &user.items {
        cg.register_item(item);
    }
    let main = user.items.iter().find_map(|i| match &i.kind {
        ItemKind::Fn(f) if f.name.name == "main" => cg.res.fn_decl.get(&NodeKey::of(f)).copied(),
        _ => None,
    });
    let Some(main) = main else {
        return Err(NativeError::plain("the program has no `fn main()`"));
    };
    let main_name = cg.request(main, Vec::new())?;
    let wrapper = cg.main_wrapper(main, &main_name)?;
    while let Some(inst) = cg.queue.pop() {
        cg.gen_fn(inst)?;
    }
    Ok(format!(
        "{}\n/* ===== generated from Tau ===== */\n{}\n{}\n{}\n{}\n",
        super::RUNTIME,
        cg.types,
        cg.protos,
        cg.bodies,
        wrapper
    ))
}

impl<'a> Cg<'a> {
    // ------------------------------------------------------------ registration

    fn register_item(&mut self, item: &'a Item) {
        match &item.kind {
            ItemKind::Fn(f) => self.register(f),
            ItemKind::Const(c) => {
                if let Some(id) = self.res.const_decl.get(&NodeKey::of(c)) {
                    self.consts.insert(*id, c);
                }
            }
            ItemKind::Impl(i) => i.items.iter().for_each(|x| self.register_item(x)),
            ItemKind::Trait(t) => t.items.iter().for_each(|x| self.register_item(x)),
            _ => {}
        }
    }

    fn register(&mut self, f: &'a FnDecl) {
        if let Some(id) = self.res.fn_decl.get(&NodeKey::of(f)) {
            self.decls.insert(*id, f);
        }
    }

    fn fresh(&mut self, base: &str) -> String {
        self.counter += 1;
        format!("{base}_{}", self.counter)
    }

    fn ty_of(&self, e: &Expr) -> Ty {
        let t = self.res.expr_ty.get(&NodeKey::of(e)).cloned().unwrap_or(Ty::Error);
        t.subst(&self.sub)
    }

    fn pat_ty(&self, p: &Pattern) -> Ty {
        let t = self.res.pat_ty.get(&NodeKey::of(p)).cloned().unwrap_or(Ty::Error);
        t.subst(&self.sub)
    }

    fn is_unit(t: &Ty) -> bool {
        t.is_unit()
    }

    fn is_never(t: &Ty) -> bool {
        matches!(t, Ty::Never)
    }

    fn apply_sub(&self, subst: &[(GenericId, Ty)]) -> Subst {
        subst.iter().map(|(g, t)| (*g, t.subst(&self.sub))).collect()
    }

    // ------------------------------------------------------------ types

    fn adt_fields(&self, id: AdtId, args: &[Ty]) -> Vec<Vec<Ty>> {
        let def = self.prog.adt(id);
        let sub: Subst = def.generics.iter().copied().zip(args.iter().cloned()).collect();
        match &def.kind {
            AdtKind::Struct(fs) => vec![fs.iter().map(|f| f.ty.subst(&sub)).collect()],
            AdtKind::Enum(vs) => vs.iter().map(|v| v.fields.iter().map(|t| t.subst(&sub)).collect()).collect(),
        }
    }

    fn is_enum(&self, id: AdtId) -> bool {
        matches!(self.prog.adt(id).kind, AdtKind::Enum(_))
    }

    fn mangle(&mut self, ty: &Ty) -> CgResult<String> {
        Ok(match ty {
            Ty::Int(i) => i.name().to_string(),
            Ty::Float(f) => f.name().to_string(),
            Ty::Bool => "bool".to_string(),
            Ty::Char => "char".to_string(),
            Ty::Str => "str".to_string(),
            Ty::Never => "unit".to_string(),
            Ty::Tuple(ts) if ts.is_empty() => "unit".to_string(),
            Ty::Tuple(ts) => {
                let parts: CgResult<Vec<String>> = ts.iter().map(|t| self.mangle(t)).collect();
                format!("T{}_{}", ts.len(), parts?.join("_"))
            }
            Ty::Fn(ps, r) => {
                let parts: CgResult<Vec<String>> = ps.iter().map(|t| self.mangle(t)).collect();
                format!("F{}_{}_{}", ps.len(), parts?.join("_"), self.mangle(r)?)
            }
            Ty::Array(t, Some(n)) => format!("A{n}_{}", self.mangle(t)?),
            Ty::Slice(t) => format!("S_{}", self.mangle(t)?),
            Ty::Adt(id, args) => {
                let def = self.prog.adt(*id);
                if def.module == 0 {
                    match def.name.as_str() {
                        "String" => return Ok("String".to_string()),
                        "Vec" => return Ok(format!("Vec_{}", self.mangle(&args[0])?)),
                        "Range" | "Ptr" | "Task" | "Cluster" | "TaskPanicked" => {
                            return unsupported(self.describe(ty));
                        }
                        _ => {}
                    }
                }
                if let Some((k, v)) = self.map_kv(ty) {
                    return Ok(format!("Map_{}_{}", self.mangle(&k)?, self.mangle(&v)?));
                }
                let mut s = format!("{}{}", def.name, id.0);
                for a in args {
                    s.push('_');
                    s.push_str(&self.mangle(a)?);
                }
                s
            }
            other => return unsupported(self.describe(other)),
        })
    }

    fn describe(&self, ty: &Ty) -> String {
        match ty {
            Ty::Complex(_) => "complex numbers (milestone 4)".to_string(),
            Ty::Array(..) => "arrays (milestone 2)".to_string(),
            Ty::Slice(_) => "slices (milestone 2)".to_string(),
            Ty::Tensor(..) => "tensors (milestone 4)".to_string(),
            Ty::Adt(id, _) => format!("the type `{}` (milestone 2)", self.prog.adt(*id).name),
            Ty::Fn(..) => "function values and closures (milestone 3)".to_string(),
            Ty::Param(_) => "generic code with unresolved parameters".to_string(),
            other => format!("the type {other:?}"),
        }
    }

    fn cty(&mut self, ty: &Ty) -> CgResult<String> {
        Ok(match ty {
            Ty::Int(it) => match it {
                IntTy::I8 => "int8_t",
                IntTy::I16 => "int16_t",
                IntTy::I32 => "int32_t",
                IntTy::I64 | IntTy::Isize => "int64_t",
                IntTy::I128 => "__int128",
                IntTy::U8 => "uint8_t",
                IntTy::U16 => "uint16_t",
                IntTy::U32 => "uint32_t",
                IntTy::U64 | IntTy::Usize => "uint64_t",
                IntTy::U128 => "unsigned __int128",
            }
            .to_string(),
            Ty::Float(FloatTy::F32) => "float".to_string(),
            Ty::Float(FloatTy::F64) => "double".to_string(),
            Ty::Bool => "bool".to_string(),
            Ty::Char => "uint32_t".to_string(),
            Ty::Str => "tau_str".to_string(),
            Ty::Never => "tau_unit".to_string(),
            Ty::Tuple(ts) if ts.is_empty() => "tau_unit".to_string(),
            Ty::Tuple(ts) => {
                let key = self.mangle(ty)?;
                if let Some(n) = self.type_names.get(&key) {
                    return Ok(n.clone());
                }
                let name = format!("tup_{key}");
                // dependencies first, so the definition order is always valid C
                let mut fields = String::new();
                for (i, t) in ts.iter().enumerate() {
                    let ct = self.cty(t)?;
                    fields.push_str(&format!(" {ct} f{i};"));
                }
                self.types.push_str(&format!("typedef struct {{{fields} }} {name};\n"));
                self.type_names.insert(key, name.clone());
                name
            }
            Ty::Fn(ps, r) => {
                let key = self.mangle(ty)?;
                if let Some(nm) = self.type_names.get(&key) {
                    return Ok(nm.clone());
                }
                let rt = self.cty(r)?;
                let mut plist = String::from("void *");
                for p in ps {
                    plist.push_str(", ");
                    plist.push_str(&self.cty(p)?);
                }
                let name = format!("fnv_{key}");
                self.types.push_str(&format!("typedef struct {{ void *env; {rt} (*call)({plist}); void (*drop)(void *); }} {name};\n"));
                self.type_names.insert(key, name.clone());
                name
            }
            Ty::Array(t, Some(n)) => {
                let key = self.mangle(ty)?;
                if let Some(nm) = self.type_names.get(&key) {
                    return Ok(nm.clone());
                }
                let et = self.cty(t)?;
                let name = format!("arr_{key}");
                self.types.push_str(&format!("typedef struct {{ {et} v[{}]; }} {name};\n", (*n).max(1)));
                self.type_names.insert(key, name.clone());
                name
            }
            Ty::Slice(t) => {
                let key = self.mangle(ty)?;
                if let Some(nm) = self.type_names.get(&key) {
                    return Ok(nm.clone());
                }
                let et = self.cty(t)?;
                let name = format!("sl_{key}");
                self.types.push_str(&format!("typedef struct {{ {et} *ptr; size_t len; }} {name};\n"));
                self.type_names.insert(key, name.clone());
                name
            }
            Ty::Adt(id, args) if self.is_builtin_adt(*id, "String") => {
                let _ = args;
                "tau_buf".to_string()
            }
            t if self.map_kv(t).is_some() => {
                let (k, v) = self.map_kv(t).unwrap();
                let key = self.mangle(ty)?;
                if let Some(nm) = self.type_names.get(&key) {
                    return Ok(nm.clone());
                }
                let (kt, vt) = (self.cty(&k)?, self.cty(&v)?);
                let name = format!("map_{key}");
                self.types.push_str(&format!("typedef struct {{ {kt} *keys; {vt} *vals; size_t len, cap; int64_t *tab; size_t tabcap; }} {name};\n"));
                self.type_names.insert(key, name.clone());
                name
            }
            Ty::Adt(id, args) if self.is_builtin_adt(*id, "Vec") => {
                let key = self.mangle(ty)?;
                if let Some(nm) = self.type_names.get(&key) {
                    return Ok(nm.clone());
                }
                let et = self.cty(&args[0])?;
                let name = format!("vec_{key}");
                self.types.push_str(&format!("typedef struct {{ {et} *ptr; size_t len, cap; }} {name};\n"));
                self.type_names.insert(key, name.clone());
                name
            }
            Ty::Adt(id, args) => {
                let key = self.mangle(ty)?;
                if let Some(n) = self.type_names.get(&key) {
                    return Ok(n.clone());
                }
                let all = self.adt_fields(*id, args);
                let is_enum = self.is_enum(*id);
                let name = format!("{}_{key}", if is_enum { "en" } else { "st" });
                let mut text = String::new();
                if is_enum {
                    let mut union = String::new();
                    for (vi, fields) in all.iter().enumerate() {
                        if fields.is_empty() {
                            continue;
                        }
                        let mut fs = String::new();
                        for (i, t) in fields.iter().enumerate() {
                            let ct = self.cty(t)?;
                            fs.push_str(&format!(" {ct} f{i};"));
                        }
                        union.push_str(&format!(" struct {{{fs} }} v{vi};"));
                    }
                    if union.is_empty() {
                        text.push_str(&format!("typedef struct {{ uint32_t tag; }} {name};\n"));
                    } else {
                        text.push_str(&format!("typedef struct {{ uint32_t tag; union {{{union} }} u; }} {name};\n"));
                    }
                } else {
                    let mut fs = String::new();
                    for (i, t) in all[0].iter().enumerate() {
                        let ct = self.cty(t)?;
                        fs.push_str(&format!(" {ct} f{i};"));
                    }
                    if fs.is_empty() {
                        fs.push_str(" uint8_t unit_;");
                    }
                    text.push_str(&format!("typedef struct {{{fs} }} {name};\n"));
                }
                self.types.push_str(&text);
                self.type_names.insert(key, name.clone());
                name
            }
            other => return unsupported(self.describe(other)),
        })
    }

    fn is_builtin_adt(&self, id: AdtId, name: &str) -> bool {
        let d = self.prog.adt(id);
        d.module == 0 && d.name == name
    }

    /// `Map<K, V>` (declared in `std.map`)
    fn map_kv(&self, ty: &Ty) -> Option<(Ty, Ty)> {
        match ty {
            Ty::Adt(id, args) => {
                let d = self.prog.adt(*id);
                if d.name == "Map" && self.mod_paths.get(d.module).map(|p| p == "std.map").unwrap_or(false) {
                    Some((args[0].clone(), args[1].clone()))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// `IoError` (declared in the prelude)
    fn is_io_error(&self, ty: &Ty) -> bool {
        matches!(ty, Ty::Adt(id, _) if self.is_builtin_adt(*id, "IoError"))
    }

    /// The `main` wrapper the runtime calls: runs `main`, and if it returned `Err`, prints
    /// `error: ...` to stderr and exits with status 1 (as `tau run` does).
    fn main_wrapper(&mut self, main: FnId, main_name: &str) -> CgResult<String> {
        let ret = self.prog.fn_(main).sig.ret.clone();
        if let Ty::Adt(id, args) = &ret {
            if self.is_builtin_adt(*id, "Result") {
                let rct = self.cty(&ret)?;
                let f = self.fmt_stmt("&m_", "r_.u.v1.f0", &args[1])?;
                return Ok(format!(
                    "static int tau_run_main(void) {{ {rct} r_ = {main_name}(); if (r_.tag == 1) {{ tau_buf m_ = {{0}}; {f} tau_flush_stdout(); (void)!write(2, \"error: \", 7); if (m_.len) (void)!write(2, m_.ptr, m_.len); (void)!write(2, \"\\n\", 1); return 1; }} return 0; }}"
                ));
            }
        }
        Ok(format!("static int tau_run_main(void) {{ (void){main_name}(); return 0; }}"))
    }

    fn vec_elem(&self, ty: &Ty) -> Option<Ty> {
        match ty {
            Ty::Adt(id, args) if self.is_builtin_adt(*id, "Vec") => Some(args[0].clone()),
            _ => None,
        }
    }

    fn is_string(&self, ty: &Ty) -> bool {
        matches!(ty, Ty::Adt(id, _) if self.is_builtin_adt(*id, "String"))
    }

    // ------------------------------------------------------------ ownership

    /// Does a value of this type own heap memory that must be freed?
    fn needs_drop(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Tuple(ts) => ts.iter().any(|t| self.needs_drop(t)),
            // a closure owns its environment
            Ty::Fn(..) => true,
            Ty::Array(t, _) => self.needs_drop(t),
            Ty::Adt(id, args) => {
                let d = self.prog.adt(*id);
                if d.module == 0 && matches!(d.name.as_str(), "String" | "Vec") {
                    return true;
                }
                if self.map_kv(ty).is_some() {
                    return true;
                }
                self.adt_fields(*id, args).iter().flatten().any(|t| self.needs_drop(t))
            }
            _ => false,
        }
    }

    /// The name of the function that frees a value of this type (and zeroes it), generating
    /// it on first use. `None` when the type owns nothing.
    fn drop_fn(&mut self, ty: &Ty) -> CgResult<Option<String>> {
        if !self.needs_drop(ty) {
            return Ok(None);
        }
        let m = self.mangle(ty)?;
        let name = format!("tau_drop_{m}");
        if !self.helpers.insert(name.clone()) {
            return Ok(Some(name));
        }
        let ct = self.cty(ty)?;
        self.protos.push_str(&format!("static void {name}({ct} *p);\n"));
        let mut body = String::new();
        match ty {
            Ty::Fn(..) => body.push_str("if (p->drop) p->drop(p->env);\n"),
            Ty::Tuple(ts) => {
                for (i, t) in ts.iter().enumerate() {
                    if let Some(f) = self.drop_fn(t)? {
                        body.push_str(&format!("{f}(&p->f{i});\n"));
                    }
                }
            }
            Ty::Array(t, Some(n)) => {
                if let Some(f) = self.drop_fn(t)? {
                    body.push_str(&format!("for (size_t i = 0; i < {n}; i++) {f}(&p->v[i]);\n"));
                }
            }
            Ty::Adt(id, args) if self.is_builtin_adt(*id, "String") => {
                let _ = args;
                body.push_str("free(p->ptr);\n");
            }
            Ty::Adt(id, args) if self.is_builtin_adt(*id, "Vec") => {
                if let Some(f) = self.drop_fn(&args[0])? {
                    body.push_str(&format!("for (size_t i = 0; i < p->len; i++) {f}(&p->ptr[i]);\n"));
                }
                body.push_str("free(p->ptr);\n");
            }
            t if self.map_kv(t).is_some() => {
                let (k, v) = self.map_kv(t).unwrap();
                if let Some(f) = self.drop_fn(&k)? {
                    body.push_str(&format!("for (size_t i = 0; i < p->len; i++) {f}(&p->keys[i]);\n"));
                }
                if let Some(f) = self.drop_fn(&v)? {
                    body.push_str(&format!("for (size_t i = 0; i < p->len; i++) {f}(&p->vals[i]);\n"));
                }
                body.push_str("free(p->keys); free(p->vals); free(p->tab);\n");
            }
            Ty::Adt(id, args) => {
                let all = self.adt_fields(*id, args);
                if self.is_enum(*id) {
                    body.push_str("switch (p->tag) {\n");
                    for (vi, fields) in all.iter().enumerate() {
                        let mut inner = String::new();
                        for (i, t) in fields.iter().enumerate() {
                            if let Some(f) = self.drop_fn(t)? {
                                inner.push_str(&format!("{f}(&p->u.v{vi}.f{i}); "));
                            }
                        }
                        if !inner.is_empty() {
                            body.push_str(&format!("case {vi}: {inner}break;\n"));
                        }
                    }
                    body.push_str("default: break;\n}\n");
                } else {
                    for (i, t) in all[0].iter().enumerate() {
                        if let Some(f) = self.drop_fn(t)? {
                            body.push_str(&format!("{f}(&p->f{i});\n"));
                        }
                    }
                }
            }
            _ => {}
        }
        body.push_str(&format!("memset(p, 0, sizeof({ct}));\n"));
        self.bodies.push_str(&format!("static void {name}({ct} *p) {{\n{body}}}\n"));
        Ok(Some(name))
    }

    /// A statement dropping the value in the C lvalue `place` (empty if the type owns nothing).
    fn drop_stmt(&mut self, place: &str, ty: &Ty) -> CgResult<String> {
        Ok(match self.drop_fn(ty)? {
            Some(f) => format!("{f}(&{place}); "),
            None => String::new(),
        })
    }

    /// Statements dropping every owning local in `scopes[from..]`, innermost first.
    fn scope_drops(&mut self, from: usize) -> CgResult<String> {
        let mut locals = Vec::new();
        for sc in self.scopes[from.min(self.scopes.len())..].iter().rev() {
            for l in sc.iter().rev() {
                if l.owns {
                    locals.push((l.cname.clone(), l.ty.clone()));
                }
            }
        }
        let mut out = String::new();
        for (c, t) in locals {
            out.push_str(&self.drop_stmt(&c, &t)?);
        }
        Ok(out)
    }

    /// Run `f` collecting the temporaries it creates; returns its result and the statements
    /// that drop those temporaries.
    fn with_temps<R>(&mut self, f: impl FnOnce(&mut Self) -> CgResult<R>) -> CgResult<(R, String)> {
        self.temps.push(Vec::new());
        let r = f(self);
        let made = self.temps.pop().unwrap_or_default();
        let r = r?;
        let mut drops = String::new();
        for (name, ty) in made.into_iter().rev() {
            drops.push_str(&self.drop_stmt(&name, &ty)?);
        }
        Ok((r, drops))
    }

    /// A hoisted, zero-initialised temporary of type `ty`, dropped at the end of the current
    /// statement. Returns its C name.
    fn new_temp(&mut self, ty: &Ty) -> CgResult<String> {
        let ct = self.cty(ty)?;
        let name = self.fresh("tmp");
        self.hoisted.push_str(&format!("  {ct} {name} = {{0}};\n"));
        if self.needs_drop(ty) {
            if let Some(last) = self.temps.last_mut() {
                last.push((name.clone(), ty.clone()));
            }
        }
        Ok(name)
    }

    /// Name used in the runtime's per-integer-type helpers (`tau_add_i64`, ...).
    fn int_suffix(it: IntTy) -> &'static str {
        it.name()
    }

    // ------------------------------------------------------------ copy and moves

    /// Is `ty` `Copy`? (Mirrors the checker: scalars, tuples and arrays of `Copy`, and ADTs
    /// with an explicit `impl Copy`.) Needed from M2b on, when values own heap memory.
    #[allow(dead_code)]
    fn is_copy(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Int(_) | Ty::Float(_) | Ty::Complex(_) | Ty::Bool | Ty::Char | Ty::Str | Ty::Never | Ty::Slice(_) => true,
            Ty::Tuple(ts) => ts.iter().all(|t| self.is_copy(t)),
            Ty::Array(t, _) => self.is_copy(t),
            Ty::Adt(..) => {
                let Some(copy) = self.prog.find_trait("Copy") else { return false };
                for imp in &self.prog.impls {
                    if imp.trait_ref.as_ref().map(|t| t.trait_) != Some(copy) {
                        continue;
                    }
                    let mut binds = Vec::new();
                    if !match_ty(&imp.self_ty, ty, &mut binds) {
                        continue;
                    }
                    let ok = binds.iter().all(|(g, t)| {
                        self.prog.generic(*g).bounds.iter().all(|b| b.trait_ != copy || self.is_copy(t))
                    });
                    if ok {
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    // ------------------------------------------------------------ functions and instances

    /// The C name of the instance of `fid` with these (already substituted) generic arguments,
    /// queueing it for generation if it is new.
    fn request(&mut self, fid: FnId, sub: Subst) -> CgResult<String> {
        let mut key = format!("{}", fid.0);
        for (g, t) in &sub {
            key.push_str(&format!("|{}={}", g.0, self.mangle(t)?));
        }
        if let Some(n) = self.instances.get(&key) {
            return Ok(n.clone());
        }
        let name = format!("tau_fn_{}_{}", self.prog.fn_(fid).name, self.instances.len());
        self.instances.insert(key, name.clone());
        self.queue.push(Inst { name: name.clone(), fid, sub });
        Ok(name)
    }

    /// The type of `self` in a method.
    fn self_ty(&self, def: &FnDef) -> Option<Ty> {
        match def.parent {
            Parent::Impl(i) => Some(self.prog.impl_(i).self_ty.subst(&self.sub)),
            Parent::Trait(t) => Some(Ty::Param(self.prog.trait_(t).self_generic).subst(&self.sub)),
            Parent::Free => None,
        }
    }

    /// Does a parameter of this mode and type travel as a pointer to the caller's value?
    fn by_ptr(mode: ParamMode, ty: &Ty) -> bool {
        mode == ParamMode::Mut && !matches!(ty, Ty::Slice(_) | Ty::Str)
    }


    fn gen_fn(&mut self, inst: Inst) -> CgResult<()> {
        let def = self.prog.fn_(inst.fid).clone();
        let Some(decl) = self.decls.get(&inst.fid).copied() else {
            return unsupported(format!("calling `{}`", def.name));
        };
        let Some(body) = &decl.body else {
            return unsupported(format!("calling `{}`", def.name));
        };
        self.sub = inst.sub.clone();
        let ret_ty = def.sig.ret.subst(&self.sub);
        let ret_c = self.cty(&ret_ty)?;
        self.scopes.clear();
        self.scopes.push(Vec::new());
        self.loops.clear();
        self.temps.clear();
        self.hoisted.clear();
        self.cur_ret = ret_ty.clone();
        let mut params = Vec::new();
        if let Some(mode) = def.sig.self_mode {
            let st = self.self_ty(&def).unwrap_or(Ty::Error);
            let ct = self.cty(&st)?;
            let cname = self.fresh("p_self");
            let ptr = Self::by_ptr(mode, &st);
            params.push(if ptr { format!("{ct} *{cname}") } else { format!("{ct} {cname}") });
            let owns = mode == ParamMode::Own && self.needs_drop(&st);
            self.scopes.last_mut().unwrap().push(Local { name: "self".to_string(), cname, ty: st, by_ptr: ptr, owns });
        }
        for (p, pd) in decl.params.iter().filter(|p| p.name.name != "self").zip(&def.sig.params) {
            let ty = pd.ty.subst(&self.sub);
            let ct = self.cty(&ty)?;
            let cname = self.fresh(&format!("p_{}", p.name.name));
            let ptr = Self::by_ptr(pd.mode, &ty);
            params.push(if ptr { format!("{ct} *{cname}") } else { format!("{ct} {cname}") });
            let owns = pd.mode == ParamMode::Own && self.needs_drop(&ty);
            self.scopes.last_mut().unwrap().push(Local { name: p.name.name.clone(), cname, ty, by_ptr: ptr, owns });
        }
        let plist = if params.is_empty() { "void".to_string() } else { params.join(", ") };
        let value = self.block_value(body)?;
        let body_ty = self.block_ty(body);
        let pdrops = self.scope_drops(0)?;
        let tail = if Self::is_unit(&ret_ty) || Self::is_never(&ret_ty) {
            format!("(void)({value}); {pdrops}return 0;")
        } else if Self::is_never(&body_ty) {
            format!("(void)({value}); tau_panic_cstr(\"unreachable\");")
        } else if pdrops.is_empty() {
            format!("return {value};")
        } else {
            format!("{ret_c} r_ = {value}; {pdrops}return r_;")
        };
        self.scopes.clear();
        self.sub.clear();
        let hoisted = std::mem::take(&mut self.hoisted);
        self.protos.push_str(&format!("static {ret_c} {}({plist});\n", inst.name));
        self.bodies.push_str(&format!("static {ret_c} {}({plist}) {{\n{hoisted}  {tail}\n}}\n", inst.name));
        Ok(())
    }

    // ------------------------------------------------------------ scopes

    fn lookup(&self, name: &str) -> Option<Local> {
        for scope in self.scopes.iter().rev() {
            for l in scope.iter().rev() {
                if l.name == name {
                    return Some(l.clone());
                }
            }
        }
        None
    }

    fn declare(&mut self, name: &str, ty: Ty) -> Local {
        let cname = self.fresh(name);
        let l = Local { name: name.to_string(), cname, ty, by_ptr: false, owns: false };
        self.scopes.last_mut().unwrap().push(l.clone());
        l
    }

    /// Like `declare`, for a local that owns its value (a `let`, or a by-value binding out of
    /// an owned temporary): it is dropped at the end of its scope.
    fn declare_owned(&mut self, name: &str, ty: Ty) -> Local {
        let cname = self.fresh(name);
        let owns = self.needs_drop(&ty);
        let l = Local { name: name.to_string(), cname, ty, by_ptr: false, owns };
        self.scopes.last_mut().unwrap().push(l.clone());
        l
    }

    // ------------------------------------------------------------ blocks and statements

    /// The type of a block, computed the way the checker does.
    fn block_ty(&self, b: &Block) -> Ty {
        match b.stmts.last() {
            Some(Stmt::Expr(e)) => self.ty_of(e),
            _ => {
                let diverges = b.stmts.iter().any(|s| match s {
                    Stmt::Expr(e) => Self::is_never(&self.ty_of(e)),
                    Stmt::Let { init: Some(e), .. } => Self::is_never(&self.ty_of(e)),
                    _ => false,
                });
                if diverges { Ty::Never } else { Ty::unit() }
            }
        }
    }


    fn block_value(&mut self, b: &'a Block) -> CgResult<String> {
        self.scopes.push(Vec::new());
        for s in &b.stmts {
            if let Stmt::Item(item) = s {
                match &item.kind {
                    ItemKind::Fn(f) => self.register(f),
                    _ => return unsupported("items declared inside a function"),
                }
            }
        }
        let mut out = String::from("({ ");
        let n = b.stmts.len();
        let mut tail: Option<(String, String, Ty)> = None;
        for (i, s) in b.stmts.iter().enumerate() {
            match s {
                Stmt::Let { pat, init, .. } => {
                    let code = self.let_stmt(pat, init.as_ref())?;
                    out.push_str(&code);
                }
                Stmt::Expr(e) => {
                    let ety = self.ty_of(e);
                    if i + 1 == n {
                        let (v, drops) = self.with_temps(|c| c.owned(e))?;
                        tail = Some((v, drops, ety));
                    } else {
                        let (v, drops) = self.with_temps(|c| c.owned(e))?;
                        if self.needs_drop(&ety) {
                            let ct = self.cty(&ety)?;
                            let t = self.fresh("dv");
                            let d = self.drop_stmt(&t, &ety)?;
                            out.push_str(&format!("{{ {ct} {t} = {v}; {d}}} {drops}\n"));
                        } else {
                            out.push_str(&format!("(void)({v}); {drops}\n"));
                        }
                    }
                }
                Stmt::Item(_) => {}
            }
        }
        let sdrops = self.scope_drops(self.scopes.len() - 1)?;
        self.scopes.pop();
        match tail {
            None => {
                out.push_str(&sdrops);
                out.push_str("((tau_unit)0); })");
            }
            Some((v, drops, ety)) => {
                if drops.is_empty() && sdrops.is_empty() {
                    out.push_str(&v);
                    out.push_str("; })");
                } else if Self::is_unit(&ety) || Self::is_never(&ety) {
                    out.push_str(&format!("(void)({v}); {drops}{sdrops}((tau_unit)0); }})"));
                } else {
                    let ct = self.cty(&ety)?;
                    let r = self.fresh("bv");
                    out.push_str(&format!("{ct} {r} = {v}; {drops}{sdrops}{r}; }})"));
                }
            }
        }
        Ok(out)
    }


    fn let_stmt(&mut self, pat: &'a Pattern, init: Option<&'a Expr>) -> CgResult<String> {
        let ty = self.pat_ty(pat);
        let ct = self.cty(&ty)?;
        match init {
            Some(e) => {
                if matches!(pat.kind, PatKind::Wild) && self.is_place(e) {
                    // `let _ = place` neither moves nor drops it
                    let v = self.expr(e)?;
                    return Ok(format!("(void)({v});\n"));
                }
                let (v, drops) = self.with_temps(|c| c.convert(e, &ty))?;
                match &pat.kind {
                    PatKind::Ident { name, .. } => {
                        let l = self.declare_owned(name, ty);
                        Ok(format!("{ct} {} = {v}; {drops}\n", l.cname))
                    }
                    PatKind::Wild => {
                        let t = self.fresh("dv");
                        let d = self.drop_stmt(&t, &ty)?;
                        Ok(format!("{{ {ct} {t} = {v}; {d}}} {drops}\n"))
                    }
                    _ => {
                        let tmp = self.declare_owned("let", ty.clone());
                        let mut s = format!("{ct} {} = {v}; {drops}\n", tmp.cname);
                        self.bind_irrefutable(pat, &tmp.cname, &ty, &mut s)?;
                        Ok(s)
                    }
                }
            }
            None => match &pat.kind {
                PatKind::Ident { name, .. } => {
                    let l = self.declare_owned(name, ty);
                    Ok(format!("{ct} {} = ({ct}){{0}};\n", l.cname))
                }
                _ => unsupported("an uninitialised destructuring `let`"),
            },
        }
    }


    fn bind_irrefutable(&mut self, pat: &'a Pattern, access: &str, ty: &Ty, out: &mut String) -> CgResult<()> {
        match &pat.kind {
            PatKind::Wild => Ok(()),
            PatKind::Ident { name, .. } => {
                let ct = self.cty(ty)?;
                // the value is moved out of the (owned) source, which is zeroed in that place
                let l = self.declare_owned(name, ty.clone());
                out.push_str(&format!("{ct} {} = {access};\n", l.cname));
                if l.owns {
                    out.push_str(&format!("memset(&{access}, 0, sizeof({ct}));\n"));
                }
                Ok(())
            }
            PatKind::Tuple(ps) => {
                let Ty::Tuple(ts) = ty else { return unsupported("this pattern") };
                for (i, (p, t)) in ps.iter().zip(ts).enumerate() {
                    self.bind_irrefutable(p, &format!("{access}.f{i}"), t, out)?;
                }
                Ok(())
            }
            PatKind::Variant { args, .. } => {
                let Ty::Adt(id, targs) = ty else { return unsupported("this pattern") };
                if self.is_enum(*id) {
                    return unsupported("a refutable pattern in `let`");
                }
                let fields = self.adt_fields(*id, targs).remove(0);
                for (i, (p, t)) in args.iter().zip(&fields).enumerate() {
                    self.bind_irrefutable(p, &format!("{access}.f{i}"), t, out)?;
                }
                Ok(())
            }
            _ => unsupported("this kind of pattern in `let`"),
        }
    }

    // ------------------------------------------------------------ expression helpers


    fn place(&mut self, e: &'a Expr) -> CgResult<(String, Ty)> {
        match &e.kind {
            ExprKind::Ident(name) => {
                let Some(l) = self.lookup(name) else { return unsupported(format!("assigning to `{name}`")) };
                Ok((if l.by_ptr { format!("(*{})", l.cname) } else { l.cname.clone() }, l.ty))
            }
            ExprKind::MutBorrow(inner) => self.place(inner),
            ExprKind::Index { base, indices } if indices.len() == 1 && self.map_kv(&self.ty_of(base)).is_some() => {
                let insert = std::mem::take(&mut self.insert_ctx);
                let bt = self.ty_of(base);
                let (kt, vt) = self.map_kv(&bt).unwrap();
                let (b, _) = self.place(base)?;
                if insert {
                    // `m[k] = v` and `m[k] += v` create a missing entry (starting from zero)
                    let slot = self.map_helper(&bt, "slot")?;
                    let key = self.convert(&indices[0], &kt)?;
                    Ok((format!("(*{slot}(&{b}, {key}))"), vt))
                } else {
                    let get = self.map_helper(&bt, "get")?;
                    let key = self.borrowed_as(&indices[0], &kt)?;
                    Ok((format!("(*{get}(&{b}, {key}))"), vt))
                }
            }
            ExprKind::Index { base, indices } if indices.len() == 1 && !matches!(indices[0].kind, ExprKind::Range { .. }) => {
                let bt = self.ty_of(base);
                let elem = match &bt {
                    Ty::Array(t, Some(_)) | Ty::Slice(t) => (**t).clone(),
                    other => match self.vec_elem(other) {
                        Some(t) => t,
                        None => return unsupported("indexing this type (milestone 4)"),
                    },
                };
                let (b, _) = self.place(base)?;
                let view = self.view(&b, &bt)?;
                let vt = self.cty(&Ty::Slice(Box::new(elem.clone())))?;
                let it = self.expr(&indices[0])?;
                Ok((
                    format!("(*({{ {vt} s_ = {view}; __int128 i_ = (__int128)({it}); if (i_ < 0 || (uint64_t)i_ >= s_.len) tau_oob(s_.len, i_); &s_.ptr[i_]; }}))"),
                    elem,
                ))
            }
            ExprKind::Field { base, name } => {
                let (b, bt) = self.place(base)?;
                match &bt {
                    Ty::Tuple(ts) => match name.name.parse::<usize>() {
                        Ok(i) if i < ts.len() => Ok((format!("{b}.f{i}"), ts[i].clone())),
                        _ => unsupported("assigning to this field"),
                    },
                    Ty::Adt(id, args) if !self.is_enum(*id) && self.prog.adt(*id).module != 0 => {
                        let def = self.prog.adt(*id);
                        let AdtKind::Struct(fs) = &def.kind else { return unsupported("this field") };
                        let Some(i) = fs.iter().position(|f| f.name == name.name) else {
                            return unsupported(format!("the field `{}`", name.name));
                        };
                        let ft = self.adt_fields(*id, args).remove(0).remove(i);
                        Ok((format!("{b}.f{i}"), ft))
                    }
                    _ => unsupported("assigning to this field"),
                }
            }
            // any other expression: evaluate it into a temporary and use that
            _ => {
                let ty = self.ty_of(e);
                let v = self.expr(e)?;
                let t = self.new_temp(&ty)?;
                Ok((format!("(*({{ {t} = {v}; &{t}; }}))"), ty))
            }
        }
    }


    /// Is this expression a place (a variable, or a field or element of one)? Reading a place
    /// does not give an owned value: moving out of it has to zero it.
    fn is_place(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Ident(n) => self.lookup(n).is_some(),
            ExprKind::MutBorrow(i) => self.is_place(i),
            ExprKind::Field { base, .. } => {
                !matches!(self.res.expr_res.get(&NodeKey::of(e)), Some(Res::Const(_) | Res::Variant(..) | Res::Fn(..))) && self.is_place(base)
            }
            ExprKind::Index { base, indices } => indices.len() == 1 && !matches!(indices[0].kind, ExprKind::Range { .. }) && self.is_place(base),
            _ => false,
        }
    }

    /// The value of `e` as an owned value: if it is a place holding something that owns heap
    /// memory, the value is moved out and the place zeroed (so it is not dropped twice).
    fn owned(&mut self, e: &'a Expr) -> CgResult<String> {
        let ty = self.ty_of(e);
        if self.needs_drop(&ty) && self.is_place(e) {
            let (place, _) = self.place(e)?;
            let ct = self.cty(&ty)?;
            return Ok(format!("({{ {ct} mv_ = {place}; memset(&{place}, 0, sizeof({ct})); mv_; }})"));
        }
        self.expr(e)
    }

    /// `e` for reading only (an argument passed by borrow, a receiver, an operand): a place is
    /// used in place; a computed value that owns memory goes into a temporary dropped at the
    /// end of the statement.
    fn borrow(&mut self, e: &'a Expr) -> CgResult<String> {
        let ty = self.ty_of(e);
        if self.needs_drop(&ty) && !self.is_place(e) {
            let v = self.expr(e)?;
            let t = self.new_temp(&ty)?;
            return Ok(format!("(*({{ {t} = {v}; &{t}; }}))"));
        }
        self.expr(e)
    }

    /// A view (slice or `str`) of the C lvalue `place` of type `ty`.
    fn view(&mut self, place: &str, ty: &Ty) -> CgResult<String> {
        Ok(match ty {
            Ty::Slice(_) | Ty::Str => place.to_string(),
            Ty::Array(t, Some(n)) => {
                let st = self.cty(&Ty::Slice(t.clone()))?;
                format!("(({st}){{ ({place}).v, {n} }})")
            }
            // the place is evaluated once (it may be a computed temporary)
            t if self.is_string(t) => format!("({{ __auto_type vp_ = &({place}); (tau_str){{ vp_->ptr, vp_->len }}; }})"),
            t => match self.vec_elem(t) {
                Some(el) => {
                    let st = self.cty(&Ty::Slice(Box::new(el)))?;
                    format!("({{ __auto_type vp_ = &({place}); ({st}){{ vp_->ptr, vp_->len }}; }})")
                }
                None => return unsupported(format!("viewing {} as a slice", self.describe(t))),
            },
        })
    }

    /// Does a value of type `from` convert implicitly to `to` by taking a view?
    fn coerces_to_view(&self, from: &Ty, to: &Ty) -> bool {
        match (from, to) {
            (Ty::Array(a, _), Ty::Slice(b)) => a == b,
            (f, Ty::Slice(b)) => self.vec_elem(f).map(|e| e == **b).unwrap_or(false),
            (f, Ty::Str) => self.is_string(f),
            _ => false,
        }
    }

    /// `e` as an owned value of type `to`, applying the implicit conversions the checker allows.
    fn convert(&mut self, e: &'a Expr, to: &Ty) -> CgResult<String> {
        let from = self.ty_of(e);
        if self.coerces_to_view(&from, to) {
            let b = self.borrow(e)?;
            return self.view(&b, &from);
        }
        self.owned(e)
    }

    /// `e` passed to a parameter of type `to`: by borrow (a view if the types differ).
    fn borrowed_as(&mut self, e: &'a Expr, to: &Ty) -> CgResult<String> {
        let from = self.ty_of(e);
        if self.coerces_to_view(&from, to) {
            let b = self.borrow(e)?;
            return self.view(&b, &from);
        }
        self.borrow(e)
    }

    fn is_trivial(e: &Expr) -> bool {
        matches!(e.kind, ExprKind::Int { .. } | ExprKind::Float { .. } | ExprKind::Bool(_) | ExprKind::Char(_))
    }


    fn ordered(&mut self, es: &[&'a Expr]) -> CgResult<(String, Vec<String>)> {
        self.ordered_with(es, None, false)
    }

    /// Operands evaluated left to right: read-only (`owned == false`) or as owned values
    /// converted to `tys` (for building aggregates). Returns declarations to run first and the
    /// operand texts to use afterwards. Literals are used in place; if fewer than two operands
    /// could have side effects the order cannot matter.
    fn ordered_with(&mut self, es: &[&'a Expr], tys: Option<&[Ty]>, owned: bool) -> CgResult<(String, Vec<String>)> {
        let mut exprs = Vec::new();
        for (i, e) in es.iter().enumerate() {
            let c = if owned {
                let to = tys.and_then(|t| t.get(i).cloned()).unwrap_or_else(|| self.ty_of(e));
                self.convert(e, &to)?
            } else {
                self.borrow(e)?
            };
            exprs.push(c);
        }
        let effectful = es.iter().filter(|e| !Self::is_trivial(e) && !matches!(e.kind, ExprKind::Ident(_))).count();
        let non_lit = es.iter().filter(|e| !Self::is_trivial(e)).count();
        if es.len() < 2 || effectful == 0 || non_lit < 2 {
            return Ok((String::new(), exprs));
        }
        let mut decls = String::new();
        let mut names = Vec::new();
        for (i, (e, c)) in es.iter().zip(exprs).enumerate() {
            if Self::is_trivial(e) {
                names.push(c);
            } else {
                let ty = tys.and_then(|t| t.get(i).cloned()).unwrap_or_else(|| self.ty_of(e));
                let t = self.cty(&ty)?;
                let tmp = self.fresh("op");
                decls.push_str(&format!("{t} {tmp} = {c}; "));
                names.push(tmp);
            }
        }
        Ok((decls, names))
    }

    fn wrap(decls: String, value: String) -> String {
        if decls.is_empty() { value } else { format!("({{ {decls}{value}; }})") }
    }

    fn int_literal(&mut self, value: u128, it: IntTy) -> CgResult<String> {
        let ct = self.cty(&Ty::Int(it))?;
        Ok(if value <= u64::MAX as u128 {
            format!("(({ct}){value}ULL)")
        } else {
            format!("(({ct})(((unsigned __int128){}ULL << 64) | {}ULL))", (value >> 64) as u64, value as u64)
        })
    }

    // ------------------------------------------------------------ formatting


    fn fmt_stmt(&mut self, buf: &str, value: &str, ty: &Ty) -> CgResult<String> {
        Ok(match ty {
            Ty::Int(it) => format!("tau_fmt_{}({buf}, {value});", Self::int_suffix(*it)),
            Ty::Float(FloatTy::F64) => format!("tau_fmt_f64({buf}, {value});"),
            Ty::Float(FloatTy::F32) => format!("tau_fmt_f32({buf}, {value});"),
            Ty::Bool => format!("tau_fmt_bool({buf}, {value});"),
            Ty::Char => format!("tau_fmt_char({buf}, {value});"),
            Ty::Str => format!("tau_fmt_str({buf}, {value});"),
            Ty::Never => format!("tau_fmt_unit({buf}, {value});"),
            Ty::Tuple(ts) if ts.is_empty() => format!("tau_fmt_unit({buf}, {value});"),
            t if self.is_string(t) => format!("{{ tau_buf *sv_ = &({value}); tau_buf_push({buf}, sv_->ptr, sv_->len); }}"),
            Ty::Tuple(_) | Ty::Array(..) | Ty::Slice(_) | Ty::Adt(..) => {
                let name = self.fmt_helper(ty)?;
                format!("{name}({buf}, {value});")
            }
            other => return unsupported(format!("printing {}", self.describe(other))),
        })
    }

    /// The implementation of trait method `method` of the (prelude) trait `trait_name` for the
    /// concrete type `ty`, if there is one.
    fn trait_impl_fn(&self, trait_name: &str, method: &str, ty: &Ty) -> Option<(FnId, Subst)> {
        let tr = self.prog.find_trait(trait_name)?;
        for imp in &self.prog.impls {
            if imp.trait_ref.as_ref().map(|t| t.trait_) != Some(tr) {
                continue;
            }
            let mut binds = Vec::new();
            if !match_ty(&imp.self_ty, ty, &mut binds) {
                continue;
            }
            if let Some((_, f)) = imp.methods.iter().find(|(n, _)| n == method) {
                return Some((*f, binds));
            }
        }
        None
    }

    /// Make sure the printing function for a tuple, array, slice, `Vec` or user type exists;
    /// returns its name.
    fn fmt_helper(&mut self, ty: &Ty) -> CgResult<String> {
        let m = self.mangle(ty)?;
        let ct = self.cty(ty)?;
        let name = format!("tau_fmt_{m}");
        if !self.helpers.insert(name.clone()) {
            return Ok(name);
        }
        self.protos.push_str(&format!("static void {name}(tau_buf *b, {ct} v);\n"));
        let mut body = String::new();
        match ty {
            Ty::Tuple(ts) => {
                body.push_str("tau_buf_char(b, '(');\n");
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        body.push_str("tau_buf_cstr(b, \", \");\n");
                    }
                    body.push_str(&self.fmt_stmt("b", &format!("v.f{i}"), t)?);
                    body.push('\n');
                }
                if ts.len() == 1 {
                    body.push_str("tau_buf_char(b, ',');\n");
                }
                body.push_str("tau_buf_char(b, ')');\n");
            }
            Ty::Array(..) | Ty::Slice(_) | Ty::Adt(..) if self.seq_elem(ty).is_some() => {
                let (el, ptr, len) = self.seq_parts(ty, "v")?;
                let f = self.fmt_stmt("b", "x_", &el)?;
                let ect = self.cty(&el)?;
                body.push_str(&format!(
                    "tau_buf_char(b, '[');\nfor (size_t i = 0; i < {len}; i++) {{ if (i > 0) tau_buf_cstr(b, \", \"); {ect} x_ = {ptr}[i]; {f} }}\ntau_buf_char(b, ']');\n"
                ));
            }
            t if self.is_io_error(t) => {
                body.push_str("tau_buf_cstr(b, \"io error: \"); tau_buf_push(b, v.f0.ptr, v.f0.len);\n");
            }
            Ty::Adt(..) => {
                // a user type: call its `Display` implementation, which returns a String
                let Some((fid, binds)) = self.trait_impl_fn("Display", "fmt", ty) else {
                    return unsupported(format!("printing {} (it has no Display impl)", self.describe(ty)));
                };
                let fname = self.request(fid, binds)?;
                body.push_str(&format!("tau_buf s_ = {fname}(v); tau_buf_push(b, s_.ptr, s_.len); free(s_.ptr);\n"));
            }
            _ => return unsupported(format!("printing {}", self.describe(ty))),
        }
        self.bodies.push_str(&format!("static void {name}(tau_buf *b, {ct} v) {{\n{body}}}\n"));
        Ok(name)
    }

    /// Element type if `ty` is a sequence (array, slice, `Vec`).
    fn seq_elem(&self, ty: &Ty) -> Option<Ty> {
        match ty {
            Ty::Array(t, Some(_)) | Ty::Slice(t) => Some((**t).clone()),
            t => self.vec_elem(t),
        }
    }

    /// (element type, pointer expression, length expression) of the sequence value `v`.
    fn seq_parts(&mut self, ty: &Ty, v: &str) -> CgResult<(Ty, String, String)> {
        Ok(match ty {
            Ty::Array(t, Some(n)) => ((**t).clone(), format!("{v}.v"), format!("{n}")),
            Ty::Slice(t) => ((**t).clone(), format!("{v}.ptr"), format!("{v}.len")),
            t => match self.vec_elem(t) {
                Some(el) => (el, format!("{v}.ptr"), format!("{v}.len")),
                None => return unsupported("this sequence"),
            },
        })
    }

    /// Make sure the `eq`/`cmp` helper for a sequence type exists; returns its name.
    fn seq_helper(&mut self, ty: &Ty, kind: &str) -> CgResult<String> {
        let m = self.mangle(ty)?;
        let ct = self.cty(ty)?;
        let name = format!("tau_{kind}_{m}");
        if !self.helpers.insert(name.clone()) {
            return Ok(name);
        }
        let (el, pa, la) = self.seq_parts(ty, "a")?;
        let (_, pb, lb) = self.seq_parts(ty, "b")?;
        if kind == "eq" {
            self.protos.push_str(&format!("static bool {name}({ct} a, {ct} b);\n"));
            let e = self.eq_expr(&format!("{pa}[i]"), &format!("{pb}[i]"), &el)?;
            self.bodies.push_str(&format!("static bool {name}({ct} a, {ct} b) {{ if ({la} != {lb}) return 0; for (size_t i = 0; i < {la}; i++) if (!{e}) return 0; return 1; }}\n"));
        } else {
            self.protos.push_str(&format!("static int {name}({ct} a, {ct} b);\n"));
            let c = self.cmp_expr(&format!("{pa}[i]"), &format!("{pb}[i]"), &el)?;
            self.bodies.push_str(&format!(
                "static int {name}({ct} a, {ct} b) {{ size_t n = {la} < {lb} ? {la} : {lb}; for (size_t i = 0; i < n; i++) {{ int c = {c}; if (c != 0) return c; }} return ({la} > {lb}) - ({la} < {lb}); }}\n"
            ));
        }
        Ok(name)
    }

    /// Make sure the `fmt`/`eq`/`cmp` helper for a tuple type exists; returns its name.
    fn tuple_helper(&mut self, ty: &Ty, kind: &str) -> CgResult<String> {
        let Ty::Tuple(ts) = ty else { return unsupported("this tuple operation") };
        let m = self.mangle(ty)?;
        let ct = self.cty(ty)?;
        let name = format!("tau_{kind}_{m}");
        if !self.helpers.insert(name.clone()) {
            return Ok(name);
        }
        let ts = ts.clone();
        match kind {
            "fmt" => {
                self.protos.push_str(&format!("static void {name}(tau_buf *b, {ct} v);\n"));
                let mut body = String::from("tau_buf_char(b, '(');\n");
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        body.push_str("tau_buf_cstr(b, \", \");\n");
                    }
                    body.push_str(&self.fmt_stmt("b", &format!("v.f{i}"), t)?);
                    body.push('\n');
                }
                if ts.len() == 1 {
                    body.push_str("tau_buf_char(b, ',');\n");
                }
                body.push_str("tau_buf_char(b, ')');\n");
                self.bodies.push_str(&format!("static void {name}(tau_buf *b, {ct} v) {{\n{body}}}\n"));
            }
            "eq" => {
                self.protos.push_str(&format!("static bool {name}({ct} a, {ct} b);\n"));
                let mut parts = Vec::new();
                for (i, t) in ts.iter().enumerate() {
                    parts.push(self.eq_expr(&format!("a.f{i}"), &format!("b.f{i}"), t)?);
                }
                self.bodies.push_str(&format!("static bool {name}({ct} a, {ct} b) {{ return {}; }}\n", parts.join(" && ")));
            }
            _ => {
                self.protos.push_str(&format!("static int {name}({ct} a, {ct} b);\n"));
                let mut body = String::from("int c;\n");
                for (i, t) in ts.iter().enumerate() {
                    let c = self.cmp_expr(&format!("a.f{i}"), &format!("b.f{i}"), t)?;
                    body.push_str(&format!("c = {c}; if (c != 0) return c;\n"));
                }
                body.push_str("return 0;\n");
                self.bodies.push_str(&format!("static int {name}({ct} a, {ct} b) {{\n{body}}}\n"));
            }
        }
        Ok(name)
    }


    fn eq_expr(&mut self, a: &str, b: &str, ty: &Ty) -> CgResult<String> {
        Ok(match ty {
            Ty::Int(_) | Ty::Float(_) | Ty::Bool | Ty::Char => format!("({a} == {b})"),
            Ty::Str => format!("tau_str_eq({a}, {b})"),
            Ty::Never => "1".to_string(),
            Ty::Tuple(ts) if ts.is_empty() => "1".to_string(),
            Ty::Tuple(_) => {
                let n = self.tuple_helper(ty, "eq")?;
                format!("{n}({a}, {b})")
            }
            t if self.is_string(t) => format!("tau_str_eq((tau_str){{ ({a}).ptr, ({a}).len }}, (tau_str){{ ({b}).ptr, ({b}).len }})"),
            t if self.seq_elem(t).is_some() => {
                let n = self.seq_helper(ty, "eq")?;
                format!("{n}({a}, {b})")
            }
            Ty::Adt(..) => match self.trait_impl_fn("Eq", "eq", ty) {
                Some((fid, binds)) => format!("{}({a}, {b})", self.request(fid, binds)?),
                None => return unsupported(format!("comparing {} (it has no Eq impl)", self.describe(ty))),
            },
            other => return unsupported(format!("comparing {}", self.describe(other))),
        })
    }


    fn cmp_expr(&mut self, a: &str, b: &str, ty: &Ty) -> CgResult<String> {
        Ok(match ty {
            Ty::Int(_) | Ty::Bool | Ty::Char => format!("(({a} > {b}) - ({a} < {b}))"),
            Ty::Float(_) => format!("(({a}) < ({b}) ? -1 : ({a}) > ({b}) ? 1 : ({a}) == ({b}) ? 0 : 2)"),
            Ty::Str => format!("tau_str_cmp({a}, {b})"),
            Ty::Never => "0".to_string(),
            Ty::Tuple(ts) if ts.is_empty() => "0".to_string(),
            Ty::Tuple(_) => {
                let n = self.tuple_helper(ty, "cmp")?;
                format!("{n}({a}, {b})")
            }
            t if self.is_string(t) => format!("tau_str_cmp((tau_str){{ ({a}).ptr, ({a}).len }}, (tau_str){{ ({b}).ptr, ({b}).len }})"),
            t if self.seq_elem(t).is_some() => {
                let n = self.seq_helper(ty, "cmp")?;
                format!("{n}({a}, {b})")
            }
            Ty::Adt(..) => match self.trait_impl_fn("Ord", "lt", ty) {
                Some((fid, binds)) => {
                    let f = self.request(fid, binds)?;
                    format!("({f}({a}, {b}) ? -1 : {f}({b}, {a}) ? 1 : 0)")
                }
                None => return unsupported(format!("ordering {} (it has no Ord impl)", self.describe(ty))),
            },
            other => return unsupported(format!("ordering {}", self.describe(other))),
        })
    }

    // ------------------------------------------------------------ expressions

    fn expr(&mut self, e: &'a Expr) -> CgResult<String> {
        let ty = self.ty_of(e);
        match &e.kind {
            ExprKind::Int { value, .. } => match &ty {
                Ty::Int(it) => self.int_literal(*value, *it),
                Ty::Float(FloatTy::F32) => Ok(format!("((float)(double){value}.0)")),
                Ty::Float(FloatTy::F64) => Ok(format!("((double){value}.0)")),
                other => unsupported(self.describe(other)),
            },
            ExprKind::Float { value, .. } => match &ty {
                Ty::Float(FloatTy::F32) => Ok(format!("((float){value:?})")),
                Ty::Float(FloatTy::F64) => Ok(format!("((double){value:?})")),
                other => unsupported(self.describe(other)),
            },
            ExprKind::Bool(b) => Ok(if *b { "true" } else { "false" }.to_string()),
            ExprKind::Char(c) => Ok(format!("((uint32_t){}u)", *c as u32)),
            ExprKind::Str(pieces) => self.string_expr(pieces),
            ExprKind::Ident(name) => self.ident(e, name),
            ExprKind::Tuple(es) => {
                if es.is_empty() {
                    return Ok("((tau_unit)0)".to_string());
                }
                let ct = self.cty(&ty)?;
                let refs: Vec<&'a Expr> = es.iter().collect();
                let tys = match &ty {
                    Ty::Tuple(ts) => ts.clone(),
                    _ => Vec::new(),
                };
                let (decls, names) = self.ordered_with(&refs, Some(&tys), true)?;
                Ok(Self::wrap(decls, format!("(({ct}){{{}}})", names.join(", "))))
            }
            ExprKind::Unary { op, expr } => self.unary(e, &ty, *op, expr),
            ExprKind::Binary { op, lhs, rhs } => self.binary(e, *op, lhs, rhs),
            ExprKind::Assign { op, lhs, rhs } => self.assign(e, *op, lhs, rhs),
            ExprKind::Cast { expr, .. } => {
                let from = self.ty_of(expr);
                let v = self.expr(expr)?;
                self.cast(&v, &from, &ty)
            }
            ExprKind::Call { callee, args } => self.call(e, callee, args),
            ExprKind::MethodCall { recv, args, .. } => self.method_call(e, recv, args),
            ExprKind::Field { base, name } => self.field(e, base, &name.name),
            ExprKind::Block(b) | ExprKind::Unsafe(b) => self.block_value(b),
            ExprKind::If { cond, then, else_ } => self.if_expr(&ty, cond, then, else_.as_deref()),
            ExprKind::Match { scrutinee, arms } => self.match_expr(&ty, scrutinee, arms),
            ExprKind::Loop(b) => self.loop_expr(&ty, b),
            ExprKind::While { cond, body } => self.while_expr(cond, body),
            ExprKind::For { par, pat, iter, guard, body } => {
                if *par {
                    return unsupported("`par for` (milestone 5)");
                }
                self.for_expr(pat, iter, guard.as_deref(), body)
            }
            ExprKind::Return(v) => {
                let ret = self.cur_ret.clone();
                let drops = self.scope_drops(0)?;
                let val = match v {
                    Some(x) => {
                        let c = self.convert(x, &ret)?;
                        if Self::is_unit(&ret) || Self::is_never(&ret) {
                            format!("(void)({c}); {drops}return 0;")
                        } else {
                            let ct = self.cty(&ret)?;
                            format!("{ct} rv_ = {c}; {drops}return rv_;")
                        }
                    }
                    None => format!("{drops}return 0;"),
                };
                Ok(format!("({{ {val} ((tau_unit)0); }})"))
            }
            ExprKind::Break(v) => {
                let Some(ctx) = self.loops.last() else { return unsupported("`break` here") };
                let (brk, result, depth) = (ctx.brk.clone(), ctx.result.clone(), ctx.depth);
                let assign = match (v, result) {
                    (Some(x), Some(r)) => {
                        let c = self.owned(x)?;
                        format!("{r} = {c}; ")
                    }
                    (Some(x), None) => {
                        let c = self.owned(x)?;
                        format!("(void)({c}); ")
                    }
                    _ => String::new(),
                };
                let drops = self.scope_drops(depth)?;
                Ok(format!("({{ {assign}{drops}goto {brk}; ((tau_unit)0); }})"))
            }
            ExprKind::Continue => {
                let Some(ctx) = self.loops.last() else { return unsupported("`continue` here") };
                let (cont, depth) = (ctx.cont.clone(), ctx.depth);
                let drops = self.scope_drops(depth)?;
                Ok(format!("({{ {drops}goto {cont}; ((tau_unit)0); }})"))
            }
            ExprKind::Try(inner) => self.try_expr(&ty, inner),
            ExprKind::MutBorrow(inner) => self.expr(inner),
            ExprKind::Imag(_) => unsupported("complex numbers (milestone 4)"),
            ExprKind::Array(es) => self.array_literal(&ty, es),
            ExprKind::ArrayRepeat { value, .. } => self.array_repeat(&ty, value),
            ExprKind::Index { base, indices } => self.index_expr(e, &ty, base, indices),
            ExprKind::Closure { params, body, .. } => self.closure_expr(e, params, body, &ty),
            ExprKind::Range { .. } => unsupported("ranges outside a `for` loop or an index"),
            ExprKind::ParReduce { .. } | ExprKind::Spawn(_) | ExprKind::On { .. } => unsupported("parallel constructs (milestone 5)"),
            ExprKind::TypeApp { .. } => unsupported("this expression"),
        }
    }

    fn ident(&mut self, e: &'a Expr, name: &str) -> CgResult<String> {
        if let Some(l) = self.lookup(name) {
            return Ok(if l.by_ptr { format!("(*{})", l.cname) } else { l.cname });
        }
        match self.res.expr_res.get(&NodeKey::of(e)) {
            Some(Res::Const(cid)) => self.const_ref(*cid),
            Some(Res::Variant(adt, i, _)) => {
                let ty = self.ty_of(e);
                if matches!(ty, Ty::Fn(..)) {
                    return self.ctor_value(*adt, Some(*i), &ty);
                }
                self.construct(*adt, Some(*i), &ty, Vec::new())
            }
            Some(Res::Fn(fid, subst)) => {
                let subst = self.apply_sub(subst);
                let ty = self.ty_of(e);
                self.fn_value(*fid, &subst, &ty)
            }
            _ => unsupported(format!("`{name}` used as a value")),
        }
    }

    fn const_ref(&mut self, cid: ConstId) -> CgResult<String> {
        let def = self.prog.consts[cid.idx()].clone();
        if def.module == 0 {
            return Ok(match def.name.as_str() {
                "PI" => "((double)0x1.921fb54442d18p+1)".to_string(),
                "TAU" => "((double)0x1.921fb54442d18p+2)".to_string(),
                "E" => "((double)0x1.5bf0a8b145769p+1)".to_string(),
                other => return unsupported(format!("the built-in constant `{other}`")),
            });
        }
        let Some(decl) = self.consts.get(&cid).copied() else {
            return unsupported("a `cluster` declaration");
        };
        let name = format!("tau_const_{}_{}", def.name, cid.0);
        if self.seen_consts.insert(cid) {
            let ct = self.cty(&def.ty)?;
            let saved_scopes = std::mem::replace(&mut self.scopes, vec![Vec::new()]);
            let saved_loops = std::mem::take(&mut self.loops);
            let saved_sub = std::mem::take(&mut self.sub);
            let saved_hoisted = std::mem::take(&mut self.hoisted);
            let saved_temps = std::mem::take(&mut self.temps);
            let v = self.expr(&decl.value);
            let hoisted = std::mem::replace(&mut self.hoisted, saved_hoisted);
            self.temps = saved_temps;
            self.scopes = saved_scopes;
            self.loops = saved_loops;
            self.sub = saved_sub;
            let v = v?;
            self.protos.push_str(&format!("static {ct} {name}(void);\n"));
            self.bodies.push_str(&format!(
                "static {ct} {name}(void) {{\n{hoisted}  static bool done; static {ct} v; if (!done) {{ v = {v}; done = true; }} return v;\n}}\n"
            ));
        }
        Ok(format!("{name}()"))
    }

    fn string_expr(&mut self, pieces: &'a [StrPiece]) -> CgResult<String> {
        if let [StrPiece::Lit(s)] = pieces {
            return Ok(c_str_literal(s));
        }
        if pieces.is_empty() {
            return Ok(c_str_literal(""));
        }
        let b = self.fresh("sb");
        let mut s = format!("({{ tau_buf {b}_v = {{0}}; tau_buf *{b} = &{b}_v; ");
        s.push_str(&self.interp_into(&b, pieces)?);
        s.push_str(&format!("tau_str {b}_r = tau_intern({b}_v.ptr, {b}_v.len); free({b}_v.ptr); {b}_r; }})"));
        Ok(s)
    }

    /// Statements that append the pieces of an interpolated string to the buffer `buf`.
    fn interp_into(&mut self, buf: &str, pieces: &'a [StrPiece]) -> CgResult<String> {
        let mut s = String::new();
        for p in pieces {
            match p {
                StrPiece::Lit(l) => {
                    if !l.is_empty() {
                        s.push_str(&format!("tau_buf_push({buf}, {}, {});\n", c_bytes_literal(l), l.len()));
                    }
                }
                StrPiece::Interp(inner) => {
                    let ty = self.ty_of(inner);
                    let ct = self.cty(&ty)?;
                    let v = self.borrow(inner)?;
                    let tmp = self.fresh("fv");
                    s.push_str(&format!("{{ {ct} {tmp} = {v}; "));
                    s.push_str(&self.fmt_stmt(buf, &tmp, &ty)?);
                    s.push_str(" }\n");
                }
            }
        }
        Ok(s)
    }

    // ------------------------------------------------------------ constructors and fields

    /// Build a struct or enum value of type `ty` from already compiled field values.
    fn construct(&mut self, adt: AdtId, variant: Option<usize>, ty: &Ty, fields: Vec<String>) -> CgResult<String> {
        let ct = self.cty(ty)?;
        Ok(match variant {
            Some(i) => {
                if fields.is_empty() {
                    format!("(({ct}){{ .tag = {i} }})")
                } else {
                    format!("(({ct}){{ .tag = {i}, .u.v{i} = {{ {} }} }})", fields.join(", "))
                }
            }
            None => {
                let _ = adt;
                if fields.is_empty() {
                    format!("(({ct}){{ 0 }})")
                } else {
                    format!("(({ct}){{ {} }})", fields.join(", "))
                }
            }
        })
    }


    /// `[a, b, c]`
    fn array_literal(&mut self, ty: &Ty, es: &'a [Expr]) -> CgResult<String> {
        let Ty::Array(t, Some(n)) = ty else { return unsupported("an array of unknown length") };
        let ct = self.cty(ty)?;
        if es.is_empty() {
            return Ok(format!("(({ct}){{ {{ 0 }} }})"));
        }
        let refs: Vec<&'a Expr> = es.iter().collect();
        let tys: Vec<Ty> = (0..es.len()).map(|_| (**t).clone()).collect();
        let (decls, names) = self.ordered_with(&refs, Some(&tys), true)?;
        let _ = n;
        let r = self.fresh("arr");
        let mut s = format!("({{ {decls}{ct} {r} = {{ {{ 0 }} }}; ");
        for (i, v) in names.iter().enumerate() {
            s.push_str(&format!("{r}.v[{i}] = {v}; "));
        }
        s.push_str(&format!("{r}; }})"));
        Ok(s)
    }

    /// `[value; N]`
    fn array_repeat(&mut self, ty: &Ty, value: &'a Expr) -> CgResult<String> {
        let Ty::Array(t, Some(n)) = ty else { return unsupported("an array of unknown length") };
        if self.needs_drop(t) {
            return unsupported("repeating a value that owns memory");
        }
        let ct = self.cty(ty)?;
        let et = self.cty(t)?;
        let v = self.expr(value)?;
        Ok(format!("({{ {et} v_ = {v}; {ct} r_ = {{ {{ 0 }} }}; for (size_t i_ = 0; i_ < {n}; i_++) r_.v[i_] = v_; r_; }})"))
    }

    /// `vec[a, b, c]`
    fn vec_literal(&mut self, ty: &Ty, es: &'a [Expr]) -> CgResult<String> {
        let Some(el) = self.vec_elem(ty) else { return unsupported("this `vec` literal") };
        let ct = self.cty(ty)?;
        if es.is_empty() {
            return Ok(format!("(({ct}){{0}})"));
        }
        let et = self.cty(&el)?;
        let refs: Vec<&'a Expr> = es.iter().collect();
        let tys: Vec<Ty> = (0..es.len()).map(|_| el.clone()).collect();
        let (decls, names) = self.ordered_with(&refs, Some(&tys), true)?;
        let n = es.len();
        let r = self.fresh("vl");
        let mut s = format!("({{ {decls}{ct} {r} = {{0}}; {r}.ptr = ({et} *)malloc({n} * sizeof({et})); {r}.len = {r}.cap = {n}; ");
        for (i, v) in names.iter().enumerate() {
            s.push_str(&format!("{r}.ptr[{i}] = {v}; "));
        }
        s.push_str(&format!("{r}; }})"));
        Ok(s)
    }

    /// `base[i]`, `base[a..b]`, `vec[...]`, or an overloaded index.
    fn index_expr(&mut self, e: &'a Expr, ty: &Ty, base: &'a Expr, indices: &'a [Expr]) -> CgResult<String> {
        if matches!(&base.kind, ExprKind::Ident(n) if n == "vec") && self.lookup("vec").is_none() && self.res.expr_res.get(&NodeKey::of(base)).is_none() {
            return self.vec_literal(ty, indices);
        }
        if let Some(Res::Fn(fid, subst)) = self.res.expr_res.get(&NodeKey::of(e)) {
            let subst = self.apply_sub(subst);
            return self.call_resolved(*fid, &subst, None, &[base, &indices[0]]);
        }
        if indices.len() != 1 {
            return unsupported("indexing with several indices (tensors, milestone 4)");
        }
        if let ExprKind::Range { start, end, inclusive } = &indices[0].kind {
            let bt = self.ty_of(base);
            let Some(el) = self.seq_elem(&bt) else { return unsupported("slicing this type") };
            let (b, _) = self.place(base)?;
            let view = self.view(&b, &bt)?;
            let st = self.cty(&Ty::Slice(Box::new(el)))?;
            let lo = match start {
                Some(x) => format!("(__int128)({})", self.expr(x)?),
                None => "(__int128)0".to_string(),
            };
            let hi = match end {
                Some(x) => format!("(__int128)({}){}", self.expr(x)?, if *inclusive { " + 1" } else { "" }),
                None => "(__int128)s_.len".to_string(),
            };
            return Ok(format!(
                "({{ {st} s_ = {view}; __int128 a_ = {lo}; __int128 b_ = {hi}; if (b_ > (__int128)s_.len) b_ = (__int128)s_.len; if (a_ < 0 || a_ > b_) tau_slice_oob(a_, b_, s_.len); ({st}){{ s_.ptr + a_, (size_t)(b_ - a_) }}; }})"
            ));
        }
        let bt = self.ty_of(base);
        if self.seq_elem(&bt).is_none() {
            return unsupported("indexing this type (milestone 4)");
        }
        let (p, _) = self.place(e)?;
        Ok(p)
    }

    fn field(&mut self, e: &'a Expr, base: &'a Expr, name: &str) -> CgResult<String> {
        match self.res.expr_res.get(&NodeKey::of(e)) {
            Some(Res::Const(cid)) => return self.const_ref(*cid),
            Some(Res::Variant(adt, i, _)) => {
                let ty = self.ty_of(e);
                if matches!(ty, Ty::Fn(..)) {
                    return self.ctor_value(*adt, Some(*i), &ty);
                }
                return self.construct(*adt, Some(*i), &ty, Vec::new());
            }
            Some(Res::Fn(fid, subst)) => {
                let subst = self.apply_sub(subst);
                let ty = self.ty_of(e);
                return self.fn_value(*fid, &subst, &ty);
            }
            _ => {}
        }
        let bt = self.ty_of(base);
        if name == "len" && (self.seq_elem(&bt).is_some() || self.is_string(&bt) || matches!(bt, Ty::Str)) {
            let b = self.borrow(base)?;
            return Ok(match &bt {
                Ty::Array(_, Some(n)) => format!("((void)({b}), (uint64_t){n}ULL)"),
                _ => format!("((uint64_t)({b}).len)"),
            });
        }
        let b = self.borrow(base)?;
        match &bt {
            Ty::Tuple(ts) => match name.parse::<usize>() {
                Ok(i) if i < ts.len() => Ok(format!("({b}).f{i}")),
                _ => unsupported(format!("the field `{name}`")),
            },
            Ty::Adt(id, _) if !self.is_enum(*id) => {
                let AdtKind::Struct(fs) = &self.prog.adt(*id).kind else { return unsupported("this field") };
                match fs.iter().position(|f| f.name == name) {
                    Some(i) => Ok(format!("({b}).f{i}")),
                    None => unsupported(format!("the field `{name}`")),
                }
            }
            _ => unsupported(format!("the field `{name}`")),
        }
    }

    // ------------------------------------------------------------ operators

    fn unary(&mut self, e: &'a Expr, ty: &Ty, op: UnOp, inner: &'a Expr) -> CgResult<String> {
        if let Some(Res::Fn(fid, subst)) = self.res.expr_res.get(&NodeKey::of(e)) {
            let subst = self.apply_sub(subst);
            return self.call_resolved(*fid, &subst, None, &[inner]);
        }
        // `-128i8`: negate the literal itself, as 128 does not fit in an i8
        if let (UnOp::Neg, ExprKind::Int { value, .. }, Ty::Int(it)) = (op, &inner.kind, ty) {
            let ct = self.cty(ty)?;
            return Ok(match *it {
                IntTy::U8 | IntTy::U16 | IntTy::U32 | IntTy::U64 | IntTy::Usize | IntTy::U128 => {
                    let v = self.int_literal(*value, *it)?;
                    format!("tau_neg_{}({v})", Self::int_suffix(*it))
                }
                _ if *value <= i128::MAX as u128 => format!("(({ct})(-(__int128){}))", self.int_literal(*value, IntTy::I128)?),
                _ => format!("(({ct})((__int128)1 << 127))"),
            });
        }
        let v = self.expr(inner)?;
        Ok(match (op, ty) {
            (UnOp::Not, _) => format!("(!{v})"),
            (UnOp::Neg, Ty::Int(it)) => format!("tau_neg_{}({v})", Self::int_suffix(*it)),
            (UnOp::Neg, Ty::Float(_)) => format!("(-{v})"),
            (UnOp::BitNot, Ty::Int(it)) => {
                let ct = self.cty(&Ty::Int(*it))?;
                format!("(({ct})(~{v}))")
            }
            (_, other) => return unsupported(format!("this operator on {}", self.describe(other))),
        })
    }

    fn binary(&mut self, e: &'a Expr, op: BinOp, lhs: &'a Expr, rhs: &'a Expr) -> CgResult<String> {
        let lt = self.ty_of(lhs);
        if matches!(op, BinOp::And | BinOp::Or) {
            let (l, r) = (self.expr(lhs)?, self.expr(rhs)?);
            return Ok(format!("({l} {} {r})", if op == BinOp::And { "&&" } else { "||" }));
        }
        if let Some(Res::Fn(fid, subst)) = self.res.expr_res.get(&NodeKey::of(e)) {
            // an operator implemented by a user type: the same mapping the interpreter uses
            let subst = self.apply_sub(subst);
            let name = self.prog.fn_(*fid).name.clone();
            let call = |this: &mut Self, a: &'a Expr, b: &'a Expr| this.call_resolved(*fid, &subst, None, &[a, b]);
            return match (name.as_str(), op) {
                ("eq", BinOp::Eq) => call(self, lhs, rhs),
                ("eq", _) => Ok(format!("(!{})", call(self, lhs, rhs)?)),
                ("lt", BinOp::Lt) => call(self, lhs, rhs),
                ("lt", BinOp::Gt) => call(self, rhs, lhs),
                ("lt", BinOp::Le) => Ok(format!("(!{})", call(self, rhs, lhs)?)),
                ("lt", _) => Ok(format!("(!{})", call(self, lhs, rhs)?)),
                _ => call(self, lhs, rhs),
            };
        }
        let (decls, n) = self.ordered(&[lhs, rhs])?;
        let (l, r) = (n[0].clone(), n[1].clone());
        let value = match op {
            BinOp::Eq => self.eq_expr(&l, &r, &lt)?,
            BinOp::Ne => format!("(!{})", self.eq_expr(&l, &r, &lt)?),
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => match &lt {
                Ty::Int(_) | Ty::Float(_) | Ty::Bool | Ty::Char => format!("({l} {} {r})", op.symbol()),
                _ => {
                    let c = self.cmp_expr(&l, &r, &lt)?;
                    let test = match op {
                        BinOp::Lt => "c_ == -1",
                        BinOp::Le => "c_ == -1 || c_ == 0",
                        BinOp::Gt => "c_ == 1",
                        _ => "c_ == 1 || c_ == 0",
                    };
                    format!("({{ int c_ = {c}; {test}; }})")
                }
            },
            _ => self.arith(op, &l, &r, &lt)?,
        };
        Ok(Self::wrap(decls, value))
    }

    fn arith(&mut self, op: BinOp, l: &str, r: &str, ty: &Ty) -> CgResult<String> {
        Ok(match ty {
            Ty::Int(it) => {
                let n = Self::int_suffix(*it);
                let ct = self.cty(ty)?;
                match op {
                    BinOp::Add => format!("tau_add_{n}({l}, {r})"),
                    BinOp::Sub => format!("tau_sub_{n}({l}, {r})"),
                    BinOp::Mul => format!("tau_mul_{n}({l}, {r})"),
                    BinOp::Div => format!("tau_div_{n}({l}, {r})"),
                    BinOp::Rem => format!("tau_rem_{n}({l}, {r})"),
                    BinOp::Pow => format!("tau_pow_{n}({l}, {r})"),
                    BinOp::Shl => format!("tau_shl_{n}({l}, {r})"),
                    BinOp::Shr => format!("tau_shr_{n}({l}, {r})"),
                    BinOp::BitAnd => format!("(({ct})({l} & {r}))"),
                    BinOp::BitOr => format!("(({ct})({l} | {r}))"),
                    BinOp::BitXor => format!("(({ct})({l} ^ {r}))"),
                    _ => return unsupported(format!("`{}` on integers", op.symbol())),
                }
            }
            Ty::Float(ft) => {
                let single = *ft == FloatTy::F32;
                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => format!("({l} {} {r})", op.symbol()),
                    BinOp::Rem => if single { format!("fmodf({l}, {r})") } else { format!("fmod({l}, {r})") },
                    BinOp::Pow => if single { format!("((float)pow((double){l}, (double){r}))") } else { format!("pow({l}, {r})") },
                    _ => return unsupported(format!("`{}` on floats", op.symbol())),
                }
            }
            Ty::Bool => match op {
                BinOp::BitAnd => format!("({l} & {r})"),
                BinOp::BitOr => format!("({l} | {r})"),
                BinOp::BitXor => format!("({l} ^ {r})"),
                _ => return unsupported(format!("`{}` on bools", op.symbol())),
            },
            other => return unsupported(format!("`{}` on {}", op.symbol(), self.describe(other))),
        })
    }


    /// The place assigned to: for `m[k]` on a map this creates a missing entry.
    fn assign_target(&mut self, lhs: &'a Expr) -> CgResult<(String, Ty)> {
        let direct_map = matches!(&lhs.kind, ExprKind::Index { base, indices } if indices.len() == 1 && self.map_kv(&self.ty_of(base)).is_some());
        self.insert_ctx = direct_map;
        let r = self.place(lhs);
        self.insert_ctx = false;
        r
    }

    fn assign(&mut self, e: &'a Expr, op: Option<BinOp>, lhs: &'a Expr, rhs: &'a Expr) -> CgResult<String> {
        let lt = self.ty_of(lhs);
        let rty = self.ty_of(rhs);
        let lct = self.cty(&lt)?;
        let rv = self.fresh("rv");
        let ptr = self.fresh("dst");
        match op {
            None => {
                let r = self.convert(rhs, &lt)?;
                let (target, _) = self.assign_target(lhs)?;
                let drop = self.drop_stmt(&format!("*{ptr}"), &lt)?;
                Ok(format!("({{ {lct} {rv} = {r}; {lct} *{ptr} = &{target}; {drop}*{ptr} = {rv}; ((tau_unit)0); }})"))
            }
            Some(bop) => {
                let rt = self.cty(&rty)?;
                let r = self.expr(rhs)?;
                let (target, _) = self.assign_target(lhs)?;
                if let Some(Res::Fn(fid, subst)) = self.res.expr_res.get(&NodeKey::of(e)) {
                    // `a += b` on a user type is `a = add(a, b)`
                    let subst = self.apply_sub(subst);
                    let cur = self.fresh("cur");
                    let call = self.call_with_values(*fid, &subst, &[(cur.clone(), lt.clone()), (rv.clone(), rty.clone())])?;
                    return Ok(format!("({{ {rt} {rv} = {r}; {lct} *{ptr} = &{target}; {lct} {cur} = *{ptr}; *{ptr} = {call}; ((tau_unit)0); }})"));
                }
                let value = self.arith(bop, &format!("(*{ptr})"), &rv, &lt)?;
                Ok(format!("({{ {rt} {rv} = {r}; {lct} *{ptr} = &{target}; *{ptr} = {value}; ((tau_unit)0); }})"))
            }
        }
    }

    fn cast(&mut self, v: &str, from: &Ty, to: &Ty) -> CgResult<String> {
        let to_c = self.cty(to)?;
        Ok(match (from, to) {
            (Ty::Int(_) | Ty::Bool, Ty::Int(_)) => format!("(({to_c})({v}))"),
            (Ty::Char, Ty::Int(_)) => format!("(({to_c})({v}))"),
            (Ty::Int(_), Ty::Float(FloatTy::F64)) => format!("((double)({v}))"),
            (Ty::Int(_), Ty::Float(FloatTy::F32)) => format!("((float)(double)({v}))"),
            (Ty::Float(_), Ty::Int(it)) => format!("tau_from_f64_{}((double)({v}))", Self::int_suffix(*it)),
            (Ty::Float(_), Ty::Float(_)) => format!("(({to_c})({v}))"),
            (Ty::Int(_), Ty::Char) => format!("tau_char_from_int((__int128)({v}))"),
            (a, b) if a == b => v.to_string(),
            (a, b) => return unsupported(format!("casting {} to {}", self.describe(a), self.describe(b))),
        })
    }

    // ------------------------------------------------------------ calls

    /// Find what a call to `fid` with these (substituted) generic arguments runs: a trait
    /// method is replaced by the implementing type's method, and a function with a body
    /// becomes a monomorphic instance.
    fn resolve(&mut self, fid: FnId, subst: &[(GenericId, Ty)]) -> CgResult<Target> {
        let (fid, subst) = self.resolve_trait_method(fid, subst);
        let def = self.prog.fn_(fid).clone();
        if def.has_body && self.decls.contains_key(&fid) {
            let name = self.request(fid, subst.clone())?;
            return Ok(Target::User { name, fid, sub: subst });
        }
        let Some(path) = self.mod_paths.get(def.module).cloned() else {
            return unsupported(format!("calling `{}`", def.name));
        };
        let key = match def.parent {
            Parent::Impl(i) => {
                let head = match &self.prog.impl_(i).self_ty {
                    Ty::Adt(a, _) => self.prog.adt(*a).name.clone(),
                    Ty::Slice(_) => "slice".to_string(),
                    Ty::Str => "str".to_string(),
                    _ => "?".to_string(),
                };
                format!("{head}::{}", def.name)
            }
            _ if path == "prelude" => def.name.clone(),
            _ => format!("{path}::{}", def.name),
        };
        Ok(Target::Builtin { key, fid, sub: subst })
    }

    /// If `fid` is a trait method and `Self` is a known type, the impl's version (the same
    /// lookup the interpreter does at run time, done once here).
    fn resolve_trait_method(&self, fid: FnId, subst: &[(GenericId, Ty)]) -> (FnId, Subst) {
        let def = self.prog.fn_(fid);
        let Parent::Trait(tid) = def.parent else { return (fid, subst.to_vec()) };
        let tdef = self.prog.trait_(tid);
        let Some(self_ty) = subst.iter().find(|(g, _)| *g == tdef.self_generic).map(|(_, t)| t.clone()) else {
            return (fid, subst.to_vec());
        };
        if matches!(self_ty, Ty::Param(_) | Ty::Var(_) | Ty::Error) {
            return (fid, subst.to_vec());
        }
        for imp in &self.prog.impls {
            if imp.trait_ref.as_ref().map(|t| t.trait_) != Some(tid) {
                continue;
            }
            let mut binds: Subst = Vec::new();
            if !match_ty(&imp.self_ty, &self_ty, &mut binds) {
                continue;
            }
            let Some((_, ifid)) = imp.methods.iter().find(|(n, _)| *n == def.name) else {
                // a default method: instantiated with `Self` bound
                return (fid, subst.to_vec());
            };
            let isig = &self.prog.fn_(*ifid).sig;
            for (tg, ig) in def.sig.generics.iter().zip(&isig.generics) {
                if let Some((_, t)) = subst.iter().find(|(g, _)| g == tg) {
                    binds.push((*ig, t.clone()));
                }
            }
            return (*ifid, binds);
        }
        (fid, subst.to_vec())
    }

    fn call(&mut self, e: &'a Expr, callee: &'a Expr, args: &'a [Expr]) -> CgResult<String> {
        match self.res.expr_res.get(&NodeKey::of(callee)) {
            Some(Res::Fn(fid, subst)) => {
                let subst = self.apply_sub(subst);
                let refs: Vec<&'a Expr> = args.iter().collect();
                self.call_resolved(*fid, &subst, None, &refs)
            }
            Some(Res::StructCtor(adt, _)) => self.construct_call(e, *adt, None, args),
            Some(Res::Variant(adt, i, _)) => self.construct_call(e, *adt, Some(*i), args),
            _ => self.call_value(callee, args),
        }
    }

    /// `f(x)` where `f` is a function value (a closure or a function used as a value).
    fn call_value(&mut self, callee: &'a Expr, args: &'a [Expr]) -> CgResult<String> {
        let fty = self.ty_of(callee);
        let Ty::Fn(ptys, ret) = &fty else { return unsupported("this kind of call") };
        let fct = self.cty(&fty)?;
        let f = self.borrow(callee)?;
        let _ = ret;
        let mut decls = format!("{fct} f_ = {f}; ");
        let mut names = vec!["f_.env".to_string()];
        for (i, (a, pt)) in args.iter().zip(ptys).enumerate() {
            let v = self.borrowed_as(a, pt)?;
            let ct = self.cty(pt)?;
            decls.push_str(&format!("{ct} x{i}_ = {v}; "));
            names.push(format!("x{i}_"));
        }
        Ok(format!("({{ {decls}f_.call({}); }})", names.join(", ")))
    }

    fn method_call(&mut self, e: &'a Expr, recv: &'a Expr, args: &'a [Expr]) -> CgResult<String> {
        match self.res.expr_res.get(&NodeKey::of(e)) {
            Some(Res::Fn(fid, subst)) => {
                let subst = self.apply_sub(subst);
                let has_self = self.prog.fn_(*fid).sig.self_mode.is_some();
                let refs: Vec<&'a Expr> = args.iter().collect();
                self.call_resolved(*fid, &subst, if has_self { Some(recv) } else { None }, &refs)
            }
            Some(Res::StructCtor(adt, _)) => self.construct_call(e, *adt, None, args),
            Some(Res::Variant(adt, i, _)) => self.construct_call(e, *adt, Some(*i), args),
            Some(Res::Builtin(_)) => unsupported("tensors (milestone 4)"),
            _ => unsupported("this method call"),
        }
    }

    fn construct_call(&mut self, e: &'a Expr, adt: AdtId, variant: Option<usize>, args: &'a [Expr]) -> CgResult<String> {
        let ty = self.ty_of(e);
        let refs: Vec<&'a Expr> = args.iter().collect();
        let ftys = match &ty {
            Ty::Adt(id, targs) => {
                let all = self.adt_fields(*id, targs);
                match variant {
                    Some(i) => all[i].clone(),
                    None => all[0].clone(),
                }
            }
            _ => Vec::new(),
        };
        let (decls, names) = self.ordered_with(&refs, Some(&ftys), true)?;
        let v = self.construct(adt, variant, &ty, names)?;
        Ok(Self::wrap(decls, v))
    }


    fn call_resolved(&mut self, fid: FnId, subst: &[(GenericId, Ty)], recv: Option<&'a Expr>, args: &[&'a Expr]) -> CgResult<String> {
        let target = self.resolve(fid, subst)?;
        let (tfid, tsub) = match &target {
            Target::User { fid, sub, .. } | Target::Builtin { fid, sub, .. } => (*fid, sub.clone()),
        };
        let tdef = self.prog.fn_(tfid).clone();
        let sig = tdef.sig.clone();
        // the type `self` has in the callee (a receiver may need converting to it)
        let self_target = match tdef.parent {
            Parent::Impl(i) => Some(self.prog.impl_(i).self_ty.subst(&tsub)),
            Parent::Trait(t) => Some(Ty::Param(self.prog.trait_(t).self_generic).subst(&tsub)),
            Parent::Free => None,
        };
        // operands in call order, with how each one is passed
        let mut ops: Vec<(&'a Expr, ParamMode, Ty)> = Vec::new();
        let mut it = args.iter();
        if let (Some(r), Some(mode)) = (recv, sig.self_mode) {
            let st = self_target.clone().unwrap_or_else(|| self.ty_of(r));
            ops.push((r, mode, st));
        } else if sig.self_mode.is_some() {
            // an operator function: the first operand plays the receiver
            if let Some(a) = it.next() {
                let st = self_target.clone().unwrap_or_else(|| self.ty_of(a));
                ops.push((a, sig.self_mode.unwrap(), st));
            }
        }
        for (i, a) in it.enumerate() {
            let pd = sig.params.get(i);
            let mode = pd.map(|p| p.mode).unwrap_or(ParamMode::Borrow);
            let ty = pd.map(|p| p.ty.subst(&tsub)).unwrap_or(Ty::Error);
            ops.push((*a, mode, ty));
        }
        if let Target::Builtin { key, .. } = &target {
            let ret = sig.ret.subst(&tsub);
            return self.builtin(key, &tsub, &ret, &ops);
        }
        let Target::User { name, .. } = target else { unreachable!() };
        let (decls, names) = self.ordered_args(&ops)?;
        Ok(Self::wrap(decls, format!("{name}({})", names.join(", "))))
    }

    /// Call `fid` with arguments that are already C values (named temporaries), all by value.
    fn call_with_values(&mut self, fid: FnId, subst: &[(GenericId, Ty)], vals: &[(String, Ty)]) -> CgResult<String> {
        let target = self.resolve(fid, subst)?;
        match target {
            Target::User { name, fid, .. } => {
                let sig = self.prog.fn_(fid).sig.clone();
                let mut names = Vec::new();
                for (i, (v, t)) in vals.iter().enumerate() {
                    let mode = if i == 0 { sig.self_mode } else { sig.params.get(i - 1).map(|p| p.mode) };
                    if mode == Some(ParamMode::Mut) && Self::by_ptr(ParamMode::Mut, t) {
                        names.push(format!("&{v}"));
                    } else {
                        names.push(v.clone());
                    }
                }
                Ok(format!("{name}({})", names.join(", ")))
            }
            Target::Builtin { key, .. } => unsupported(format!("the library operator `{key}`")),
        }
    }


    fn ordered_args(&mut self, ops: &[(&'a Expr, ParamMode, Ty)]) -> CgResult<(String, Vec<String>)> {
        let mut decls = String::new();
        let mut names = Vec::new();
        let temps = ops.len() >= 2 && ops.iter().filter(|o| !Self::is_trivial(o.0) && !matches!(o.0.kind, ExprKind::Ident(_))).count() >= 1 && ops.iter().filter(|o| !Self::is_trivial(o.0)).count() >= 2;
        for (a, mode, pty) in ops {
            if Self::by_ptr(*mode, pty) {
                names.push(self.mut_ptr(a)?);
                continue;
            }
            // `mut x` at the call site only marks the borrow; `x` is what is evaluated
            let a: &'a Expr = match &a.kind {
                ExprKind::MutBorrow(x) => x,
                _ => a,
            };
            let v = match mode {
                ParamMode::Own => self.convert(a, pty)?,
                _ => self.borrowed_as(a, pty)?,
            };
            if temps && !Self::is_trivial(a) {
                let t = self.cty(pty)?;
                let tmp = self.fresh("arg");
                decls.push_str(&format!("{t} {tmp} = {v}; "));
                names.push(tmp);
            } else {
                names.push(v);
            }
        }
        Ok((decls, names))
    }

    /// A pointer to the place `a` (the argument of a `mut` parameter).
    fn mut_ptr(&mut self, a: &'a Expr) -> CgResult<String> {
        let inner: &'a Expr = match &a.kind {
            ExprKind::MutBorrow(x) => x,
            _ => a,
        };
        let (place, _) = self.place(inner)?;
        // a `mut` parameter is already a pointer: hand it on as is
        Ok(match (&inner.kind, place.strip_prefix("(*").and_then(|p| p.strip_suffix(')'))) {
            (ExprKind::Ident(_), Some(p)) => p.to_string(),
            _ => format!("&{place}"),
        })
    }


    fn builtin(&mut self, key: &str, subst: &[(GenericId, Ty)], ret: &Ty, ops: &[(&'a Expr, ParamMode, Ty)]) -> CgResult<String> {
        let args: Vec<&'a Expr> = ops.iter().map(|o| o.0).collect();
        let targ = subst.first().map(|(_, t)| t.clone());
        let name = key;
        match name {
            "print" => {
                let a = args[0];
                let ty = self.ty_of(a);
                if let ExprKind::Str(pieces) = &a.kind {
                    if pieces.iter().any(|p| matches!(p, StrPiece::Interp(_))) {
                        let body = self.interp_into("o_", pieces)?;
                        return Ok(format!("({{ tau_buf *o_ = tau_out_begin(); {body}tau_buf_char(o_, '\\n'); tau_out_end(); ((tau_unit)0); }})"));
                    }
                }
                let ct = self.cty(&ty)?;
                let v = self.borrow(a)?;
                let f = self.fmt_stmt("o_", "pv_", &ty)?;
                Ok(format!("({{ {ct} pv_ = {v}; tau_buf *o_ = tau_out_begin(); {f} tau_buf_char(o_, '\\n'); tau_out_end(); ((tau_unit)0); }})"))
            }
            "assert" => {
                let c = self.expr(args[0])?;
                Ok(format!("({{ if (!({c})) tau_panic_cstr(\"assertion failed\"); ((tau_unit)0); }})"))
            }
            "assert_eq" => {
                let ty = self.ty_of(args[0]);
                let ct = self.cty(&ty)?;
                let (decls, n) = self.ordered(&[args[0], args[1]])?;
                let eq = self.eq_expr("ea_", "eb_", &ty)?;
                let fa = self.fmt_stmt("&m_", "ea_", &ty)?;
                let fb = self.fmt_stmt("&m_", "eb_", &ty)?;
                Ok(format!(
                    "({{ {decls}{ct} ea_ = {}; {ct} eb_ = {}; if (!{eq}) {{ tau_buf m_ = {{0}}; tau_buf_cstr(&m_, \"assertion failed: left != right\\n  left: \"); {fa} tau_buf_cstr(&m_, \"\\n right: \"); {fb} tau_panic((const char *)m_.ptr, m_.len); }} ((tau_unit)0); }})",
                    n[0], n[1]
                ))
            }
            "panic" => {
                let m = self.borrowed_as(args[0], &Ty::Str)?;
                Ok(format!("({{ tau_panic_str({m}); ((tau_unit)0); }})"))
            }
            "sqrt" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "exp" | "ln" | "log10" | "floor" | "ceil" | "round" => {
                let single = matches!(targ, Some(Ty::Float(FloatTy::F32)));
                let f = if name == "ln" { "log" } else { name };
                let x = self.expr(args[0])?;
                Ok(if single { format!("((float){f}((double)({x})))") } else { format!("{f}({x})") })
            }
            "atan2" => {
                let single = matches!(targ, Some(Ty::Float(FloatTy::F32)));
                let (decls, n) = self.ordered(&[args[0], args[1]])?;
                let v = if single { format!("((float)atan2((double)({}), (double)({})))", n[0], n[1]) } else { format!("atan2({}, {})", n[0], n[1]) };
                Ok(Self::wrap(decls, v))
            }
            "abs" => {
                let x = self.expr(args[0])?;
                Ok(match targ {
                    Some(Ty::Float(FloatTy::F32)) => format!("fabsf({x})"),
                    Some(Ty::Float(_)) => format!("fabs({x})"),
                    Some(Ty::Int(it)) => {
                        let n = Self::int_suffix(it);
                        let ct = self.cty(&Ty::Int(it))?;
                        format!("({{ {ct} a_ = {x}; a_ < 0 ? tau_neg_{n}(a_) : a_; }})")
                    }
                    _ => return unsupported("`abs` on this type"),
                })
            }
            "min" | "max" => {
                let Some(t) = targ else { return unsupported("`min`/`max` on this type") };
                let ct = self.cty(&t)?;
                let (decls, n) = self.ordered(&[args[0], args[1]])?;
                let test = match &t {
                    Ty::Int(_) | Ty::Float(_) | Ty::Bool | Ty::Char => {
                        if name == "min" { "(a_ <= b_)".to_string() } else { "(!(a_ < b_))".to_string() }
                    }
                    _ => {
                        let c = self.cmp_expr("a_", "b_", &t)?;
                        if name == "min" { format!("({{ int c_ = {c}; c_ == -1 || c_ == 0; }})") } else { format!("({{ int c_ = {c}; !(c_ == -1); }})") }
                    }
                };
                Ok(Self::wrap(decls, format!("({{ {ct} a_ = {}; {ct} b_ = {}; {test} ? a_ : b_; }})", n[0], n[1])))
            }
            "sum" | "slice::sum" => {
                let Some(t) = targ else { return unsupported("`sum` on this type") };
                let Ty::Slice(_) = &ops[0].2 else { return unsupported("`sum` on this type") };
                let st = self.cty(&ops[0].2)?;
                let ct = self.cty(&t)?;
                let v = self.borrowed_as(ops[0].0, &ops[0].2)?;
                let zero = match &t {
                    Ty::Float(_) => "0.0".to_string(),
                    _ => "0".to_string(),
                };
                let add = self.arith(BinOp::Add, "acc_", "s_.ptr[i_]", &t)?;
                Ok(format!("({{ {st} s_ = {v}; {ct} acc_ = ({ct}){zero}; for (size_t i_ = 0; i_ < s_.len; i_++) acc_ = {add}; acc_; }})"))
            }
            "slice::map" | "slice::filter" => {
                let Ty::Slice(el) = &ops[0].2 else { return unsupported("this `map`/`filter`") };
                let Ty::Adt(_, rargs) = ret else { return unsupported("this `map`/`filter`") };
                let (st, vt) = (self.cty(&ops[0].2)?, self.cty(ret)?);
                let out_t = self.cty(&rargs[0])?;
                let fct = self.cty(&ops[1].2)?;
                let v = self.borrowed_as(ops[0].0, &ops[0].2)?;
                let f = self.borrowed_as(ops[1].0, &ops[1].2)?;
                let alloc = format!("if (s_.len) {{ r_.ptr = ({out_t} *)malloc(s_.len * sizeof({out_t})); r_.cap = s_.len; }}");
                if name == "slice::map" {
                    Ok(format!("({{ {st} s_ = {v}; {fct} f_ = {f}; {vt} r_ = {{0}}; {alloc} for (size_t i_ = 0; i_ < s_.len; i_++) r_.ptr[r_.len++] = f_.call(f_.env, s_.ptr[i_]); r_; }})"))
                } else {
                    let keep = match self.clone_fn(el)? {
                        Some(c) => format!("{c}(&s_.ptr[i_])"),
                        None => "s_.ptr[i_]".to_string(),
                    };
                    Ok(format!("({{ {st} s_ = {v}; {fct} f_ = {f}; {vt} r_ = {{0}}; {alloc} for (size_t i_ = 0; i_ < s_.len; i_++) if (f_.call(f_.env, s_.ptr[i_])) r_.ptr[r_.len++] = {keep}; r_; }})"))
                }
            }
            "slice::is_empty" => {
                let v = self.borrowed_as(ops[0].0, &ops[0].2)?;
                Ok(format!("(({v}).len == 0)"))
            }
            "slice::to_vec" => {
                let Ty::Slice(el) = &ops[0].2 else { return unsupported("`to_vec` on this type") };
                let vt = self.cty(&Ty::Adt(self.vec_adt()?, vec![(**el).clone()]))?;
                let et = self.cty(el)?;
                let v = self.borrowed_as(ops[0].0, &ops[0].2)?;
                let sty = self.cty(&ops[0].2)?;
                let copy = match self.clone_fn(el)? {
                    Some(f) => format!("for (size_t i_ = 0; i_ < s_.len; i_++) r_.ptr[i_] = {f}(&s_.ptr[i_]);"),
                    None => format!("memcpy(r_.ptr, s_.ptr, s_.len * sizeof({et}));"),
                };
                Ok(format!(
                    "({{ {sty} s_ = {v}; {vt} r_ = {{0}}; if (s_.len > 0) {{ r_.ptr = ({et} *)malloc(s_.len * sizeof({et})); {copy} r_.len = r_.cap = s_.len; }} r_; }})"
                ))
            }
            "Vec::new" => {
                let Some(el) = targ else { return unsupported("`Vec.new` here") };
                let vt = self.cty(&Ty::Adt(self.vec_adt()?, vec![el]))?;
                Ok(format!("(({vt}){{0}})"))
            }
            "Vec::push" => {
                let Some(el) = targ else { return unsupported("`push` here") };
                let f = self.vec_helper(&el, "push")?;
                let p = self.mut_ptr(ops[0].0)?;
                let x = self.convert(ops[1].0, &ops[1].2)?;
                let et = self.cty(&el)?;
                Ok(format!("({{ {}", format!("{}* v_ = {p}; {et} x_ = {x}; {f}(v_, x_); ((tau_unit)0); }})", self.cty(&Ty::Adt(self.vec_adt()?, vec![el.clone()]))?)))
            }
            "Vec::pop" => {
                let Some(el) = targ else { return unsupported("`pop` here") };
                let f = self.vec_helper(&el, "pop")?;
                let p = self.mut_ptr(ops[0].0)?;
                Ok(format!("{f}({p})"))
            }
            "std.fs::read" | "std.io::read_line" => {
                let rct = self.cty(ret)?;
                let Ty::Adt(_, rargs) = ret else { return unsupported("this library call") };
                let ect = self.cty(&rargs[1])?;
                let call = if name == "std.fs::read" {
                    let p = self.borrowed_as(ops[0].0, &Ty::Str)?;
                    format!("tau_fs_read({p}, &s_)")
                } else {
                    "tau_read_line(&s_)".to_string()
                };
                Ok(format!(
                    "({{ tau_buf s_ = {{0}}; int e_ = {call}; {rct} r_; if (e_) {{ {ect} ie_ = {{ tau_io_msg(e_) }}; r_ = ({rct}){{ .tag = 1, .u.v1 = {{ ie_ }} }}; }} else {{ r_ = ({rct}){{ .tag = 0, .u.v0 = {{ s_ }} }}; }} r_; }})"
                ))
            }
            "std.fs::write" => {
                let rct = self.cty(ret)?;
                let Ty::Adt(_, rargs) = ret else { return unsupported("this library call") };
                let ect = self.cty(&rargs[1])?;
                let (decls, n) = {
                    let p = self.borrowed_as(ops[0].0, &Ty::Str)?;
                    let d = self.borrowed_as(ops[1].0, &Ty::Str)?;
                    (String::new(), vec![p, d])
                };
                Ok(format!(
                    "({{ {decls}tau_str p_ = {}; tau_str d_ = {}; int e_ = tau_fs_write(p_, d_); {rct} r_; if (e_) {{ {ect} ie_ = {{ tau_io_msg(e_) }}; r_ = ({rct}){{ .tag = 1, .u.v1 = {{ ie_ }} }}; }} else {{ r_ = ({rct}){{ .tag = 0, .u.v0 = {{ 0 }} }}; }} r_; }})",
                    n[0], n[1]
                ))
            }
            "std.fs::exists" => {
                let p = self.borrowed_as(ops[0].0, &Ty::Str)?;
                Ok(format!("tau_fs_exists({p})"))
            }
            "Map::new" => {
                let mt = self.cty(ret)?;
                Ok(format!("(({mt}){{0}})"))
            }
            "Map::contains" | "Map::index" | "Map::top" => {
                let self_ty = ops[0].2.clone();
                let Some((kt, _)) = self.map_kv(&self_ty) else { return unsupported("this map operation") };
                let f = self.map_helper(&self_ty, if name == "Map::top" { "top" } else { "find" })?;
                let m = self.borrow(ops[0].0)?;
                let mt = self.cty(&self_ty)?;
                match name {
                    "Map::top" => {
                        let n = self.expr(ops[1].0)?;
                        Ok(format!("({{ {mt} *m_ = &({m}); uint64_t n_ = (uint64_t)({n}); {f}(m_, n_); }})"))
                    }
                    "Map::contains" => {
                        let k = self.borrowed_as(ops[1].0, &kt)?;
                        let kct = self.cty(&kt)?;
                        Ok(format!("({{ {mt} *m_ = &({m}); {kct} k_ = {k}; {f}(m_, k_) >= 0; }})"))
                    }
                    _ => {
                        let get = self.map_helper(&self_ty, "get")?;
                        let k = self.borrowed_as(ops[1].0, &kt)?;
                        let kct = self.cty(&kt)?;
                        Ok(format!("(*({{ {mt} *m_ = &({m}); {kct} k_ = {k}; {get}(m_, k_); }}))"))
                    }
                }
            }
            "String::new" => Ok("((tau_buf){0})".to_string()),
            "String::from" | "str::to_string" => {
                let v = self.borrowed_as(ops[0].0, &Ty::Str)?;
                Ok(format!("({{ tau_str s_ = {v}; tau_buf b_ = {{0}}; tau_buf_push(&b_, s_.ptr, s_.len); b_; }})"))
            }
            "String::push_str" => {
                let p = self.mut_ptr(ops[0].0)?;
                let v = self.borrowed_as(ops[1].0, &Ty::Str)?;
                Ok(format!("({{ tau_buf *b_ = {p}; tau_str s_ = {v}; tau_buf_push(b_, s_.ptr, s_.len); ((tau_unit)0); }})"))
            }
            "str::lowercase" | "str::uppercase" => {
                let v = self.borrowed_as(ops[0].0, &Ty::Str)?;
                let up = if name == "str::uppercase" { 1 } else { 0 };
                Ok(format!("tau_str_case({v}, {up})"))
            }
            "str::split_whitespace" => {
                let v = self.borrowed_as(ops[0].0, &Ty::Str)?;
                let vt = self.cty(&Ty::Adt(self.vec_adt()?, vec![Ty::Str]))?;
                let f = self.vec_helper(&Ty::Str, "push")?;
                Ok(format!("({{ tau_str s_ = {v}; {vt} r_ = {{0}}; size_t i_ = 0; while (i_ < s_.len) {{ while (i_ < s_.len && tau_is_space(s_.ptr[i_])) i_++; size_t j_ = i_; while (j_ < s_.len && !tau_is_space(s_.ptr[j_])) j_++; if (j_ > i_) {f}(&r_, (tau_str){{ s_.ptr + i_, j_ - i_ }}); i_ = j_; }} r_; }})"))
            }
            other => unsupported(format!("the library function `{other}`")),
        }
    }


    /// An expression hashing the key `v` of type `ty`.
    fn hash_expr(&mut self, v: &str, ty: &Ty) -> CgResult<String> {
        Ok(match ty {
            Ty::Int(IntTy::I128 | IntTy::U128) => format!("tau_hash_u64((uint64_t)({v}) ^ (uint64_t)((unsigned __int128)({v}) >> 64))"),
            Ty::Int(_) | Ty::Bool | Ty::Char => format!("tau_hash_u64((uint64_t)({v}))"),
            Ty::Str => format!("tau_hash_bytes(({v}).ptr, ({v}).len)"),
            t if self.is_string(t) => format!("tau_hash_bytes(({v}).ptr, ({v}).len)"),
            Ty::Tuple(ts) if !ts.is_empty() => {
                let mut acc = String::from("0x9e3779b97f4a7c15ULL");
                for (i, t) in ts.iter().enumerate() {
                    let h = self.hash_expr(&format!("({v}).f{i}"), t)?;
                    acc = format!("tau_hash_mix({acc}, {h})");
                }
                acc
            }
            other => return unsupported(format!("{} as a map key", self.describe(other))),
        })
    }

    /// The function that clones a value of this type (a deep copy); `None` if a plain copy
    /// is already a clone.
    fn clone_fn(&mut self, ty: &Ty) -> CgResult<Option<String>> {
        if !self.needs_drop(ty) {
            return Ok(None);
        }
        let m = self.mangle(ty)?;
        let name = format!("tau_clone_{m}");
        if !self.helpers.insert(name.clone()) {
            return Ok(Some(name));
        }
        let ct = self.cty(ty)?;
        self.protos.push_str(&format!("static {ct} {name}(const {ct} *p);\n"));
        let mut body = format!("{ct} r = *p;\n");
        match ty {
            Ty::Tuple(ts) => {
                for (i, t) in ts.iter().enumerate() {
                    if let Some(f) = self.clone_fn(t)? {
                        body.push_str(&format!("r.f{i} = {f}(&p->f{i});\n"));
                    }
                }
            }
            Ty::Array(t, Some(n)) => {
                if let Some(f) = self.clone_fn(t)? {
                    body.push_str(&format!("for (size_t i = 0; i < {n}; i++) r.v[i] = {f}(&p->v[i]);\n"));
                }
            }
            t if self.is_string(t) => {
                body.push_str("r.ptr = 0; r.len = r.cap = 0; tau_buf_push(&r, p->ptr, p->len);\n");
            }
            t if self.vec_elem(t).is_some() => {
                let el = self.vec_elem(t).unwrap();
                let et = self.cty(&el)?;
                body.push_str(&format!("r.ptr = 0; r.len = r.cap = 0; if (p->len) {{ r.ptr = ({et} *)malloc(p->len * sizeof({et})); r.len = r.cap = p->len; "));
                match self.clone_fn(&el)? {
                    Some(f) => body.push_str(&format!("for (size_t i = 0; i < p->len; i++) r.ptr[i] = {f}(&p->ptr[i]); }}\n")),
                    None => body.push_str(&format!("memcpy(r.ptr, p->ptr, p->len * sizeof({et})); }}\n")),
                }
            }
            Ty::Adt(id, args) if self.map_kv(ty).is_none() => {
                let all = self.adt_fields(*id, args);
                if self.is_enum(*id) {
                    body.push_str("switch (p->tag) {\n");
                    for (vi, fields) in all.iter().enumerate() {
                        let mut inner = String::new();
                        for (i, t) in fields.iter().enumerate() {
                            if let Some(f) = self.clone_fn(t)? {
                                inner.push_str(&format!("r.u.v{vi}.f{i} = {f}(&p->u.v{vi}.f{i}); "));
                            }
                        }
                        if !inner.is_empty() {
                            body.push_str(&format!("case {vi}: {inner}break;\n"));
                        }
                    }
                    body.push_str("default: break;\n}\n");
                } else {
                    for (i, t) in all[0].iter().enumerate() {
                        if let Some(f) = self.clone_fn(t)? {
                            body.push_str(&format!("r.f{i} = {f}(&p->f{i});\n"));
                        }
                    }
                }
            }
            other => return unsupported(format!("cloning {}", self.describe(other))),
        }
        body.push_str("return r;\n");
        self.bodies.push_str(&format!("static {ct} {name}(const {ct} *p) {{\n{body}}}\n"));
        Ok(Some(name))
    }

    /// The C functions implementing a `Map<K, V>` (generated together on first use) and the
    /// name of the one asked for: `find` (index or -1), `get` (pointer, panics if missing),
    /// `slot` (pointer, inserts a zeroed entry if missing, taking ownership of the key) or `top`.
    fn map_helper(&mut self, mty: &Ty, kind: &str) -> CgResult<String> {
        let (k, v) = self.map_kv(mty).ok_or_else(|| NativeError::plain("not a map"))?;
        let m = self.mangle(mty)?;
        let base = format!("tau_map_{{}}_{m}");
        let name = |what: &str| base.replace("{}", what);
        if self.helpers.insert(format!("map_family_{m}")) {
            let mt = self.cty(mty)?;
            let (kt, vt) = (self.cty(&k)?, self.cty(&v)?);
            let hash = self.hash_expr("k", &k)?;
            let eq = self.eq_expr("m->keys[i]", "k", &k)?;
            let kdrop = self.drop_stmt("k", &k)?;
            let (find, get, slot, rehash) = (name("find"), name("get"), name("slot"), name("rehash"));
            self.protos.push_str(&format!(
                "static int64_t {find}({mt} *m, {kt} k);\nstatic {vt} *{get}({mt} *m, {kt} k);\nstatic {vt} *{slot}({mt} *m, {kt} k);\nstatic void {rehash}({mt} *m, size_t ncap);\n"
            ));
            self.bodies.push_str(&format!(
                "static int64_t {find}({mt} *m, {kt} k) {{
  if (m->len == 0) return -1;
  size_t mask = m->tabcap - 1, h = (size_t)({hash}) & mask;
  for (;;) {{ int64_t i = m->tab[h]; if (i < 0) return -1; if ({eq}) return i; h = (h + 1) & mask; }}
}}
static void {rehash}({mt} *m, size_t ncap) {{
  free(m->tab);
  m->tab = (int64_t *)malloc(ncap * sizeof(int64_t));
  if (!m->tab) tau_panic_cstr(\"out of memory\");
  for (size_t i = 0; i < ncap; i++) m->tab[i] = -1;
  m->tabcap = ncap;
  for (size_t j = 0; j < m->len; j++) {{
    {kt} k = m->keys[j];
    size_t h = (size_t)({hash}) & (ncap - 1);
    while (m->tab[h] >= 0) h = (h + 1) & (ncap - 1);
    m->tab[h] = (int64_t)j;
  }}
}}
static {vt} *{get}({mt} *m, {kt} k) {{
  int64_t i = {find}(m, k);
  if (i < 0) tau_panic_cstr(\"key not found in map\");
  return &m->vals[i];
}}
static {vt} *{slot}({mt} *m, {kt} k) {{
  int64_t i = {find}(m, k);
  if (i >= 0) {{ {kdrop}return &m->vals[i]; }}
  if (m->len == m->cap) {{
    size_t c = m->cap ? m->cap * 2 : 4;
    m->keys = ({kt} *)realloc(m->keys, c * sizeof({kt}));
    m->vals = ({vt} *)realloc(m->vals, c * sizeof({vt}));
    if (!m->keys || !m->vals) tau_panic_cstr(\"out of memory\");
    m->cap = c;
  }}
  if ((m->len + 1) * 2 > m->tabcap) {rehash}(m, m->tabcap ? m->tabcap * 2 : 8);
  m->keys[m->len] = k;
  memset(&m->vals[m->len], 0, sizeof({vt}));
  size_t h = (size_t)({hash}) & (m->tabcap - 1);
  while (m->tab[h] >= 0) h = (h + 1) & (m->tabcap - 1);
  m->tab[h] = (int64_t)m->len;
  m->len++;
  return &m->vals[m->len - 1];
}}
"
            ));
        }
        if kind == "top" && self.helpers.insert(format!("map_top_{m}")) {
            let mt = self.cty(mty)?;
            let pair = Ty::Tuple(vec![k.clone(), v.clone()]);
            let vecp = Ty::Adt(self.vec_adt()?, vec![pair.clone()]);
            let (pt, vect) = (self.cty(&pair)?, self.cty(&vecp)?);
            let by_value = self.cmp_expr("m->vals[j]", "m->vals[i]", &v)?;
            let by_key = self.cmp_expr("m->keys[i]", "m->keys[j]", &k)?;
            let kc = match self.clone_fn(&k)? {
                Some(f) => format!("{f}(&m->keys[idx[i]])"),
                None => "m->keys[idx[i]]".to_string(),
            };
            let vc = match self.clone_fn(&v)? {
                Some(f) => format!("{f}(&m->vals[idx[i]])"),
                None => "m->vals[idx[i]]".to_string(),
            };
            let (cmpf, top) = (name("topcmp"), name("top"));
            self.protos.push_str(&format!("static {vect} {top}({mt} *m, uint64_t n);\n"));
            self.bodies.push_str(&format!(
                "static int {cmpf}(const void *a, const void *b, void *ctx) {{
  {mt} *m = ({mt} *)ctx;
  size_t i = *(const size_t *)a, j = *(const size_t *)b;
  int c = {by_value};
  if (c == 0) {{ int kc = {by_key}; return kc == 2 ? 0 : kc; }}
  return c == 2 ? 0 : c;
}}
static {vect} {top}({mt} *m, uint64_t n) {{
  {vect} r = {{0}};
  if (m->len == 0 || n == 0) return r;
  size_t *idx = (size_t *)malloc(m->len * sizeof(size_t));
  for (size_t i = 0; i < m->len; i++) idx[i] = i;
  qsort_r(idx, m->len, sizeof(size_t), {cmpf}, m);
  size_t cnt = n < m->len ? (size_t)n : m->len;
  r.ptr = ({pt} *)malloc(cnt * sizeof({pt}));
  r.len = r.cap = cnt;
  for (size_t i = 0; i < cnt; i++) r.ptr[i] = ({pt}){{ {kc}, {vc} }};
  free(idx);
  return r;
}}
"
            ));
        }
        Ok(name(kind))
    }


    // ------------------------------------------------------------ closures and function values

    /// A closure: an environment holding what it captures (heap allocated, so the closure can
    /// be returned or stored) and a C function taking that environment. Shared captures are
    /// copied (nothing can change them while the closure is alive), mutable captures are
    /// pointers to the variable, and moved captures are owned by the environment.
    fn closure_expr(&mut self, e: &'a Expr, params: &'a [ClosureParam], body: &'a Expr, ty: &Ty) -> CgResult<String> {
        let Ty::Fn(ptys, ret) = ty else { return unsupported("this closure") };
        let ret = (**ret).clone();
        let fct = self.cty(ty)?;
        let caps = self.captures.get(&NodeKey::of(e)).cloned().unwrap_or_default();
        let id = self.fresh("clo");
        // the captured variables, as seen at the point where the closure is made
        struct Cap {
            name: String,
            ty: Ty,
            kind: CaptureKind,
            init: String,
        }
        let mut capv: Vec<Cap> = Vec::new();
        for (name, kind) in &caps {
            let Some(l) = self.lookup(name) else { return unsupported(format!("capturing `{name}`")) };
            let read = if l.by_ptr { format!("(*{})", l.cname) } else { l.cname.clone() };
            let init = match kind {
                CaptureKind::MutRef => if l.by_ptr { l.cname.clone() } else { format!("&{}", l.cname) },
                CaptureKind::Move if self.needs_drop(&l.ty) => {
                    let ct = self.cty(&l.ty)?;
                    format!("({{ {ct} mv_ = {read}; memset(&{read}, 0, sizeof({ct})); mv_; }})")
                }
                _ => read,
            };
            capv.push(Cap { name: name.clone(), ty: l.ty.clone(), kind: *kind, init });
        }
        // the environment type
        let env = format!("{id}_env");
        if !capv.is_empty() {
            let mut fields = String::new();
            for (j, c) in capv.iter().enumerate() {
                let ct = self.cty(&c.ty)?;
                if c.kind == CaptureKind::MutRef {
                    fields.push_str(&format!(" {ct} *c{j};"));
                } else {
                    fields.push_str(&format!(" {ct} c{j};"));
                }
            }
            self.types.push_str(&format!("typedef struct {{{fields} }} {env};\n"));
        }
        // the body, compiled as its own C function
        let saved_scopes = std::mem::replace(&mut self.scopes, vec![Vec::new()]);
        let saved_loops = std::mem::take(&mut self.loops);
        let saved_hoisted = std::mem::take(&mut self.hoisted);
        let saved_temps = std::mem::take(&mut self.temps);
        let saved_ret = std::mem::replace(&mut self.cur_ret, ret.clone());
        let result = (|| -> CgResult<(String, String)> {
            for (j, c) in capv.iter().enumerate() {
                let cname = format!("(({env} *)env_)->c{j}");
                self.scopes[0].push(Local { name: c.name.clone(), cname, ty: c.ty.clone(), by_ptr: c.kind == CaptureKind::MutRef, owns: false });
            }
            let mut plist = String::from("void *env_");
            let mut binds = String::new();
            for (i, (p, pt)) in params.iter().zip(ptys).enumerate() {
                let ct = self.cty(pt)?;
                plist.push_str(&format!(", {ct} a{i}"));
                match &p.pat.kind {
                    PatKind::Ident { name, .. } => {
                        self.scopes[0].push(Local { name: name.clone(), cname: format!("a{i}"), ty: pt.clone(), by_ptr: false, owns: false });
                    }
                    PatKind::Wild => {}
                    _ => self.pat_bind(&p.pat, &format!("a{i}"), pt, &mut binds, false)?,
                }
            }
            let v = self.convert(body, &ret)?;
            let rct = self.cty(&ret)?;
            let tail = if Self::is_unit(&ret) || Self::is_never(&ret) { format!("(void)({v}); return 0;") } else { format!("return {v};") };
            let hoisted = std::mem::take(&mut self.hoisted);
            let fname = format!("{id}_fn");
            let proto = format!("static {rct} {fname}({plist});\n");
            let def = format!("static {rct} {fname}({plist}) {{\n{hoisted}  {binds}{tail}\n}}\n");
            self.protos.push_str(&proto);
            Ok((fname, def))
        })();
        self.scopes = saved_scopes;
        self.loops = saved_loops;
        self.hoisted = saved_hoisted;
        self.temps = saved_temps;
        self.cur_ret = saved_ret;
        let (fname, def) = result?;
        self.bodies.push_str(&def);
        if capv.is_empty() {
            return Ok(format!("(({fct}){{ 0, {fname}, 0 }})"));
        }
        // dropping the environment drops what it owns
        let dname = format!("{id}_drop");
        let mut ddrops = String::new();
        for (j, c) in capv.iter().enumerate() {
            if c.kind == CaptureKind::Move {
                ddrops.push_str(&self.drop_stmt(&format!("p->c{j}"), &c.ty)?);
            }
        }
        self.protos.push_str(&format!("static void {dname}(void *e_);\n"));
        self.bodies.push_str(&format!("static void {dname}(void *e_) {{ {env} *p = ({env} *)e_; {ddrops}free(p); }}\n"));
        let mut s = format!("({{ {env} *e_ = ({env} *)malloc(sizeof({env})); ");
        for (j, c) in capv.iter().enumerate() {
            s.push_str(&format!("e_->c{j} = {}; ", c.init));
        }
        s.push_str(&format!("({fct}){{ e_, {fname}, {dname} }}; }})"));
        Ok(s)
    }

    /// A function item used as a value: wrapped so that it has the same shape as a closure.
    fn fn_value(&mut self, fid: FnId, subst: &[(GenericId, Ty)], ty: &Ty) -> CgResult<String> {
        let Ty::Fn(ptys, ret) = ty else { return unsupported("this function value") };
        let target = self.resolve(fid, subst)?;
        let Target::User { name, fid: tfid, .. } = target else {
            return unsupported("a library function used as a value");
        };
        let sig = self.prog.fn_(tfid).sig.clone();
        if sig.self_mode.is_some() || sig.params.iter().any(|p| p.mode == ParamMode::Mut) {
            return unsupported("a function with `mut` parameters used as a value");
        }
        let fct = self.cty(ty)?;
        let tname = format!("tau_tramp_{name}");
        if self.helpers.insert(tname.clone()) {
            let rct = self.cty(ret)?;
            let mut plist = String::from("void *env_");
            let mut args = Vec::new();
            for (i, pt) in ptys.iter().enumerate() {
                plist.push_str(&format!(", {} a{i}", self.cty(pt)?));
                args.push(format!("a{i}"));
            }
            self.protos.push_str(&format!("static {rct} {tname}({plist});\n"));
            self.bodies.push_str(&format!("static {rct} {tname}({plist}) {{ (void)env_; return {name}({}); }}\n", args.join(", ")));
        }
        Ok(format!("(({fct}){{ 0, {tname}, 0 }})"))
    }

    /// A struct or variant constructor used as a function value.
    fn ctor_value(&mut self, adt: AdtId, variant: Option<usize>, ty: &Ty) -> CgResult<String> {
        let Ty::Fn(ptys, ret) = ty else { return unsupported("this constructor value") };
        let fct = self.cty(ty)?;
        let rm = self.mangle(ret)?;
        let tname = format!("tau_ctor_{rm}_{}", variant.map(|v| v.to_string()).unwrap_or_default());
        if self.helpers.insert(tname.clone()) {
            let rct = self.cty(ret)?;
            let mut plist = String::from("void *env_");
            let mut args = Vec::new();
            for (i, pt) in ptys.iter().enumerate() {
                plist.push_str(&format!(", {} a{i}", self.cty(pt)?));
                args.push(format!("a{i}"));
            }
            let v = self.construct(adt, variant, ret, args)?;
            self.protos.push_str(&format!("static {rct} {tname}({plist});\n"));
            self.bodies.push_str(&format!("static {rct} {tname}({plist}) {{ (void)env_; return {v}; }}\n"));
        }
        Ok(format!("(({fct}){{ 0, {tname}, 0 }})"))
    }

    fn vec_adt(&self) -> CgResult<AdtId> {
        self.prog.find_adt("Vec").ok_or_else(|| NativeError::plain("the prelude has no Vec"))
    }

    /// The C helper `push` or `pop` for `Vec<el>`, generated on first use.
    fn vec_helper(&mut self, el: &Ty, kind: &str) -> CgResult<String> {
        let vty = Ty::Adt(self.vec_adt()?, vec![el.clone()]);
        let m = self.mangle(&vty)?;
        let name = format!("tau_vec_{kind}_{m}");
        if !self.helpers.insert(name.clone()) {
            return Ok(name);
        }
        let vt = self.cty(&vty)?;
        let et = self.cty(el)?;
        if kind == "push" {
            self.protos.push_str(&format!("static void {name}({vt} *v, {et} x);\n"));
            self.bodies.push_str(&format!(
                "static void {name}({vt} *v, {et} x) {{ if (v->len == v->cap) {{ size_t c = v->cap ? v->cap * 2 : 4; v->ptr = ({et} *)realloc(v->ptr, c * sizeof({et})); if (!v->ptr) tau_panic_cstr(\"out of memory\"); v->cap = c; }} v->ptr[v->len++] = x; }}\n"
            ));
        } else {
            let opt = Ty::Adt(self.prog.find_adt("Option").ok_or_else(|| NativeError::plain("the prelude has no Option"))?, vec![el.clone()]);
            let ot = self.cty(&opt)?;
            self.protos.push_str(&format!("static {ot} {name}({vt} *v);\n"));
            self.bodies.push_str(&format!(
                "static {ot} {name}({vt} *v) {{ if (v->len == 0) return ({ot}){{ .tag = 1 }}; return ({ot}){{ .tag = 0, .u.v0 = {{ v->ptr[--v->len] }} }}; }}\n"
            ));
        }
        Ok(name)
    }

    // ------------------------------------------------------------ `?`


    fn try_expr(&mut self, ty: &Ty, inner: &'a Expr) -> CgResult<String> {
        let it = self.ty_of(inner);
        let Ty::Adt(id, _) = &it else { return unsupported("`?` on this type") };
        let name = self.prog.adt(*id).name.clone();
        if !matches!(name.as_str(), "Option" | "Result") || self.prog.adt(*id).module != 0 {
            return unsupported("`?` on this type");
        }
        let ict = self.cty(&it)?;
        let ret = self.cur_ret.clone();
        let rct = self.cty(&ret)?;
        let Ty::Adt(rid, _) = &ret else { return unsupported("`?` in a function that does not return Option or Result") };
        let v = self.owned(inner)?;
        let t = self.fresh("try");
        let drops = self.scope_drops(0)?;
        // the failing variant is index 1 (`None`, `Err`); its payload (if any) is passed on
        let propagate = if *rid == *id && name == "Result" {
            format!("{rct} er_ = (({rct}){{ .tag = 1, .u.v1 = {{ {t}.u.v1.f0 }} }}); {drops}return er_;")
        } else {
            format!("{rct} er_ = (({rct}){{ .tag = 1 }}); {drops}return er_;")
        };
        let vct = self.cty(ty)?;
        let value = if Self::is_unit(ty) { "((tau_unit)0)".to_string() } else { format!("(({vct}){t}.u.v0.f0)") };
        Ok(format!("({{ {ict} {t} = {v}; if ({t}.tag == 1) {{ {propagate} }} {value}; }})"))
    }

    // ------------------------------------------------------------ control flow


    fn if_expr(&mut self, ty: &Ty, cond: &'a Expr, then: &'a Block, else_: Option<&'a Expr>) -> CgResult<String> {
        let c = self.cond_expr(cond)?;
        let never = Self::is_never(ty);
        let ct = self.cty(ty)?;
        let r = self.fresh("if");
        let then_never = Self::is_never(&self.block_ty(then));
        let t = self.block_value(then)?;
        let then_stmt = if never || then_never { format!("(void)({t});") } else { format!("{r} = {t};") };
        let mut s = format!("({{ {ct} {r} = ({ct}){{0}}; if ({c}) {{ {then_stmt} }}");
        if let Some(el) = else_ {
            let else_never = Self::is_never(&self.ty_of(el));
            let ev = self.expr(el)?;
            let else_stmt = if never || else_never { format!("(void)({ev});") } else { format!("{r} = {ev};") };
            s.push_str(&format!(" else {{ {else_stmt} }}"));
        }
        s.push_str(&format!(" {r}; }})"));
        Ok(s)
    }

    /// A condition, with the temporaries it creates dropped once it has been evaluated.
    fn cond_expr(&mut self, cond: &'a Expr) -> CgResult<String> {
        let (c, drops) = self.with_temps(|s| s.expr(cond))?;
        Ok(if drops.is_empty() { c } else { format!("({{ bool c_ = {c}; {drops}c_; }})") })
    }

    fn pat_has_bindings(&self, p: &Pattern, ty: &Ty) -> bool {
        match &p.kind {
            PatKind::Ident { name, mutable } => !(self.unit_variant_of(ty, name, *mutable).is_some()),
            PatKind::Tuple(ps) => ps.iter().any(|x| self.pat_has_bindings(x, &Ty::Error)),
            PatKind::Variant { args, .. } => args.iter().any(|x| self.pat_has_bindings(x, &Ty::Error)),
            PatKind::Or(alts) => alts.iter().any(|x| self.pat_has_bindings(x, ty)),
            _ => false,
        }
    }

    /// If `name` in a pattern against type `ty` names a unit variant of that enum, its index.
    fn unit_variant_of(&self, ty: &Ty, name: &str, mutable: bool) -> Option<usize> {
        if mutable {
            return None;
        }
        let Ty::Adt(id, _) = ty else { return None };
        match &self.prog.adt(*id).kind {
            AdtKind::Enum(vs) => vs.iter().position(|v| v.name == name && v.fields.is_empty()),
            _ => None,
        }
    }

    fn variant_index(&self, id: AdtId, name: &str) -> Option<usize> {
        match &self.prog.adt(id).kind {
            AdtKind::Enum(vs) => vs.iter().position(|v| v.name == name),
            AdtKind::Struct(_) => None,
        }
    }

    /// A C condition that is true when `access` (of type `ty`) matches `p`.
    fn pat_test(&mut self, p: &'a Pattern, access: &str, ty: &Ty) -> CgResult<String> {
        Ok(match &p.kind {
            PatKind::Wild => "1".to_string(),
            PatKind::Ident { name, mutable } => match self.unit_variant_of(ty, name, *mutable) {
                Some(i) => format!("({access}.tag == {i})"),
                None => "1".to_string(),
            },
            PatKind::Lit(e) => {
                let lit = self.expr(e)?;
                self.eq_expr(access, &lit, ty)?
            }
            PatKind::Range { lo, hi, inclusive } => {
                let (l, h) = (self.expr(lo)?, self.expr(hi)?);
                format!("({access} >= {l} && {access} {} {h})", if *inclusive { "<=" } else { "<" })
            }
            PatKind::Tuple(ps) => {
                let Ty::Tuple(ts) = ty else { return unsupported("this pattern") };
                let mut parts = Vec::new();
                for (i, (sub, t)) in ps.iter().zip(ts).enumerate() {
                    parts.push(self.pat_test(sub, &format!("{access}.f{i}"), t)?);
                }
                if parts.is_empty() { "1".to_string() } else { format!("({})", parts.join(" && ")) }
            }
            PatKind::Or(alts) => {
                if self.pat_has_bindings(p, ty) {
                    return unsupported("`|` patterns that bind variables");
                }
                let mut parts = Vec::new();
                for a in alts {
                    parts.push(self.pat_test(a, access, ty)?);
                }
                format!("({})", parts.join(" || "))
            }
            PatKind::Path(path) => {
                let Ty::Adt(id, _) = ty else { return unsupported("this pattern") };
                let name = &path.last().unwrap().name;
                match self.variant_index(*id, name) {
                    Some(i) => format!("({access}.tag == {i})"),
                    None => return unsupported("this pattern"),
                }
            }
            PatKind::Variant { path, args } => {
                let Ty::Adt(id, targs) = ty else { return unsupported("this pattern") };
                let name = &path.last().unwrap().name;
                let all = self.adt_fields(*id, targs);
                let (vi, fields, base) = if self.is_enum(*id) {
                    let Some(vi) = self.variant_index(*id, name) else { return unsupported("this pattern") };
                    (Some(vi), all[vi].clone(), format!("{access}.u.v{vi}"))
                } else {
                    (None, all[0].clone(), access.to_string())
                };
                let mut parts = Vec::new();
                if let Some(vi) = vi {
                    parts.push(format!("({access}.tag == {vi})"));
                }
                for (j, (sub, t)) in args.iter().zip(&fields).enumerate() {
                    let c = self.pat_test(sub, &format!("{base}.f{j}"), t)?;
                    if c != "1" {
                        parts.push(c);
                    }
                }
                if parts.is_empty() { "1".to_string() } else { format!("({})", parts.join(" && ")) }
            }
        })
    }


    fn pat_bind(&mut self, p: &'a Pattern, access: &str, ty: &Ty, out: &mut String, owned: bool) -> CgResult<()> {
        match &p.kind {
            PatKind::Ident { name, mutable } => {
                if self.unit_variant_of(ty, name, *mutable).is_some() {
                    return Ok(());
                }
                let ct = self.cty(ty)?;
                if owned && self.needs_drop(ty) {
                    // the value is moved out of the owned scrutinee, whose copy is zeroed
                    let l = self.declare_owned(name, ty.clone());
                    out.push_str(&format!("{ct} {} = {access}; memset(&{access}, 0, sizeof({ct})); ", l.cname));
                } else {
                    let l = self.declare(name, ty.clone());
                    out.push_str(&format!("{ct} {} = {access}; ", l.cname));
                }
            }
            PatKind::Tuple(ps) => {
                let Ty::Tuple(ts) = ty else { return unsupported("this pattern") };
                for (i, (sub, t)) in ps.iter().zip(ts).enumerate() {
                    self.pat_bind(sub, &format!("{access}.f{i}"), t, out, owned)?;
                }
            }
            PatKind::Variant { path, args } => {
                let Ty::Adt(id, targs) = ty else { return unsupported("this pattern") };
                let name = &path.last().unwrap().name;
                let all = self.adt_fields(*id, targs);
                let (fields, base) = if self.is_enum(*id) {
                    let Some(vi) = self.variant_index(*id, name) else { return unsupported("this pattern") };
                    (all[vi].clone(), format!("{access}.u.v{vi}"))
                } else {
                    (all[0].clone(), access.to_string())
                };
                for (j, (sub, t)) in args.iter().zip(&fields).enumerate() {
                    self.pat_bind(sub, &format!("{base}.f{j}"), t, out, owned)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Does binding this pattern take ownership of something (a binding of a type that owns memory)?
    fn pat_binds_owned(&self, p: &Pattern, ty: &Ty) -> bool {
        match &p.kind {
            PatKind::Ident { name, mutable } => self.unit_variant_of(ty, name, *mutable).is_none() && self.needs_drop(ty),
            PatKind::Tuple(ps) => match ty {
                Ty::Tuple(ts) => ps.iter().zip(ts).any(|(x, t)| self.pat_binds_owned(x, t)),
                _ => false,
            },
            PatKind::Variant { path, args } => match ty {
                Ty::Adt(id, targs) => {
                    let all = self.adt_fields(*id, targs);
                    let name = &path.last().unwrap().name;
                    let fields = if self.is_enum(*id) {
                        match self.variant_index(*id, name) {
                            Some(vi) => all[vi].clone(),
                            None => return false,
                        }
                    } else {
                        all[0].clone()
                    };
                    args.iter().zip(&fields).any(|(x, t)| self.pat_binds_owned(x, t))
                }
                _ => false,
            },
            _ => false,
        }
    }


    fn match_expr(&mut self, ty: &Ty, scrutinee: &'a Expr, arms: &'a [MatchArm]) -> CgResult<String> {
        let st = self.ty_of(scrutinee);
        let sct = self.cty(&st)?;
        let owned_src = self.needs_drop(&st) && !self.is_place(scrutinee);
        let sv = self.expr(scrutinee)?;
        let never = Self::is_never(ty);
        let ct = self.cty(ty)?;
        let (s, r, end) = (self.fresh("scrut"), self.fresh("m"), self.fresh("mend"));
        let mut out = format!("({{ {sct} {s} = {sv}; {ct} {r} = ({ct}){{0}};\n");
        for arm in arms {
            self.scopes.push(Vec::new());
            let test = self.pat_test(&arm.pat, &s, &st)?;
            if owned_src && arm.guard.is_some() && self.pat_binds_owned(&arm.pat, &st) {
                return unsupported("a match guard on a pattern that takes ownership of part of a temporary");
            }
            let mut binds = String::new();
            self.pat_bind(&arm.pat, &s, &st, &mut binds, owned_src)?;
            let guard = match &arm.guard {
                Some(g) => Some(self.cond_expr(g)?),
                None => None,
            };
            let body_never = Self::is_never(&self.ty_of(&arm.body));
            let body = self.owned(&arm.body)?;
            let assign = if never || body_never { format!("(void)({body});") } else { format!("{r} = {body};") };
            let adrops = self.scope_drops(self.scopes.len() - 1)?;
            let finish = format!("{assign} {adrops}goto {end};");
            let inner = match guard {
                Some(g) => format!("if ({g}) {{ {finish} }}"),
                None => finish,
            };
            out.push_str(&format!("if ({test}) {{ {binds}{inner} }}\n"));
            self.scopes.pop();
        }
        let sdrop = if owned_src { self.drop_stmt(&s, &st)? } else { String::new() };
        out.push_str(&format!("tau_panic_cstr(\"no match arm matched the value\");\n{end}: ; {sdrop}{r}; }})"));
        Ok(out)
    }


    fn loop_expr(&mut self, ty: &Ty, body: &'a Block) -> CgResult<String> {
        let (brk, cont) = (self.fresh("brk"), self.fresh("cont"));
        let never = Self::is_never(ty);
        let (result, decl) = if never || Self::is_unit(ty) {
            (None, String::new())
        } else {
            let ct = self.cty(ty)?;
            let r = self.fresh("lv");
            (Some(r.clone()), format!("{ct} {r} = ({ct}){{0}}; "))
        };
        let depth = self.scopes.len();
        self.loops.push(LoopCtx { brk: brk.clone(), cont: cont.clone(), depth, result: result.clone() });
        let b = self.block_value(body);
        self.loops.pop();
        let b = b?;
        let value = result.unwrap_or_else(|| "((tau_unit)0)".to_string());
        Ok(format!("({{ {decl}for (;;) {{ (void)({b}); {cont}: ; }} {brk}: ; {value}; }})"))
    }


    fn while_expr(&mut self, cond: &'a Expr, body: &'a Block) -> CgResult<String> {
        let (brk, cont) = (self.fresh("brk"), self.fresh("cont"));
        let depth = self.scopes.len();
        self.loops.push(LoopCtx { brk: brk.clone(), cont: cont.clone(), depth, result: None });
        let c = self.cond_expr(cond);
        let b = self.block_value(body);
        self.loops.pop();
        let (c, b) = (c?, b?);
        Ok(format!("({{ for (;;) {{ if (!({c})) goto {brk}; (void)({b}); {cont}: ; }} {brk}: ; ((tau_unit)0); }})"))
    }


    fn for_expr(&mut self, pat: &'a Pattern, iter: &'a Expr, guard: Option<&'a Expr>, body: &'a Block) -> CgResult<String> {
        let (inner, mut_iter): (&'a Expr, bool) = match &iter.kind {
            ExprKind::MutBorrow(i) => (i, true),
            _ => (iter, false),
        };
        if let ExprKind::Range { start: Some(start), end: Some(end), inclusive } = &inner.kind {
            return self.for_range(pat, start, end, *inclusive, guard, body);
        }
        let it_ty = self.ty_of(inner);
        let Some(elem) = self.seq_elem(&it_ty) else {
            return unsupported(format!("`for` over {}", self.describe(&it_ty)));
        };
        let place = if self.is_place(inner) { self.place(inner)?.0 } else { self.borrow(inner)? };
        let view = self.view(&place, &it_ty)?;
        let st = self.cty(&Ty::Slice(Box::new(elem.clone())))?;
        let ect = self.cty(&elem)?;
        let (s, i) = (self.fresh("seq"), self.fresh("k"));
        let (brk, cont) = (self.fresh("brk"), self.fresh("cont"));
        let depth = self.scopes.len();
        self.scopes.push(Vec::new());
        let mut bind = String::new();
        match &pat.kind {
            PatKind::Ident { name, .. } if mut_iter => {
                let cname = self.fresh(name);
                bind.push_str(&format!("{ect} *{cname} = &{s}.ptr[{i}]; "));
                self.scopes.last_mut().unwrap().push(Local { name: name.clone(), cname, ty: elem.clone(), by_ptr: true, owns: false });
            }
            PatKind::Wild => {}
            _ if mut_iter => return unsupported("a pattern in a `for ... in mut` loop"),
            _ => self.pat_bind(pat, &format!("{s}.ptr[{i}]"), &elem, &mut bind, false)?,
        }
        self.loops.push(LoopCtx { brk: brk.clone(), cont: cont.clone(), depth, result: None });
        let g = match guard {
            Some(g) => self.cond_expr(g).map(|c| format!("if (!({c})) goto {cont}; ")),
            None => Ok(String::new()),
        };
        let b = self.block_value(body);
        self.loops.pop();
        self.scopes.pop();
        let (g, b) = (g?, b?);
        Ok(format!(
            "({{ {st} {s} = {view}; for (size_t {i} = 0; {i} < {s}.len; {i}++) {{ {bind}{g}(void)({b}); {cont}: ; }} {brk}: ; ((tau_unit)0); }})"
        ))
    }

    fn for_range(&mut self, pat: &'a Pattern, start: &'a Expr, end: &'a Expr, inclusive: bool, guard: Option<&'a Expr>, body: &'a Block) -> CgResult<String> {
        let ty = self.ty_of(start);
        let Ty::Int(_) = ty else { return unsupported("a `for` range over non-integers") };
        let ct = self.cty(&ty)?;
        let (decls, n) = self.ordered(&[start, end])?;
        let (lo, hi, i) = (self.fresh("lo"), self.fresh("hi"), self.fresh("i"));
        let (brk, cont) = (self.fresh("brk"), self.fresh("cont"));
        let depth = self.scopes.len();
        self.scopes.push(Vec::new());
        let mut bind = String::new();
        match &pat.kind {
            PatKind::Ident { name, .. } => {
                let l = self.declare(name, ty.clone());
                bind.push_str(&format!("{ct} {} = {i}; ", l.cname));
            }
            PatKind::Wild => {}
            _ => return unsupported("this pattern in a `for` loop"),
        }
        self.loops.push(LoopCtx { brk: brk.clone(), cont: cont.clone(), depth, result: None });
        let g = match guard {
            Some(g) => self.cond_expr(g).map(|c| format!("if (!({c})) goto {cont}; ")),
            None => Ok(String::new()),
        };
        let b = self.block_value(body);
        self.loops.pop();
        self.scopes.pop();
        let (g, b) = (g?, b?);
        let head = if inclusive {
            format!("if ({lo} <= {hi}) for ({ct} {i} = {lo};; {i}++)")
        } else {
            format!("for ({ct} {i} = {lo}; {i} < {hi}; {i}++)")
        };
        let tail = if inclusive { format!("if ({i} == {hi}) goto {brk};") } else { String::new() };
        Ok(format!(
            "({{ {decls}{ct} {lo} = {}; {ct} {hi} = {}; {head} {{ {bind}{g}(void)({b}); {cont}: ; {tail} }} {brk}: ; ((tau_unit)0); }})",
            n[0], n[1]
        ))
    }
}

/// A C expression of type `tau_str` for a string literal.
fn c_str_literal(s: &str) -> String {
    format!("((tau_str){{ {}, {} }})", c_bytes_literal(s), s.len())
}

/// A C string literal (as `const uint8_t *`) holding the bytes of `s`.
fn c_bytes_literal(s: &str) -> String {
    let mut out = String::from("(const uint8_t *)\"");
    for b in s.bytes() {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'?' => out.push_str("\\?"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\{:03o}", b)),
        }
    }
    out.push('"');
    out
}
