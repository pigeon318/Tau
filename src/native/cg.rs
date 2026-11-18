//! C code generation from the checked AST (milestone 1: scalars, tuples, strings, functions,
//! control flow, checked arithmetic, panics and `print`).
//!
//! Tau is expression-oriented and C is not, so expressions are emitted as GNU statement
//! expressions (`({ ...; value; })`, supported by clang and gcc). Control flow uses `goto`
//! rather than C `break`/`continue`, so a Tau `break` is never captured by an enclosing
//! `switch`-like construct the generator itself introduced. Evaluation order is made
//! explicit with temporaries, since C leaves the order of operands and arguments open.
//!
//! Anything outside the supported subset is reported as "unsupported", and the caller falls
//! back to the interpreter.

use std::collections::{HashMap, HashSet};

use super::NativeError;
use crate::ast::*;
use crate::check::{CheckResult, NodeKey, Res, Results};
use crate::program::*;
use crate::types::*;

type CgResult<T> = Result<T, NativeError>;

fn unsupported<T>(what: impl Into<String>) -> CgResult<T> {
    Err(NativeError::unsupported(format!("{} is not supported by the native backend yet", what.into())))
}

#[derive(Clone)]
struct Local {
    name: String,
    cname: String,
    ty: Ty,
    /// A `mut` parameter: the C variable is a pointer to the caller's value.
    by_ptr: bool,
}

struct LoopCtx {
    brk: String,
    cont: String,
    /// The C variable holding the value of a `loop ... break value`.
    result: Option<String>,
}

pub struct Cg<'a> {
    prog: &'a Program,
    res: &'a Results,
    decls: HashMap<FnId, &'a FnDecl>,
    consts: HashMap<ConstId, &'a ConstDecl>,
    types: String,
    protos: String,
    bodies: String,
    tuple_names: HashSet<String>,
    helpers: HashSet<String>,
    queue: Vec<FnId>,
    seen: HashSet<FnId>,
    seen_consts: HashSet<ConstId>,
    counter: usize,
    scopes: Vec<Vec<Local>>,
    loops: Vec<LoopCtx>,
    cur_ret: Ty,
}

/// Translate a checked program to a complete C source file (runtime included).
pub fn translate(user: &Module, check: &CheckResult) -> Result<String, NativeError> {
    let mut cg = Cg {
        prog: &check.program,
        res: &check.results,
        decls: HashMap::new(),
        consts: HashMap::new(),
        types: String::new(),
        protos: String::new(),
        bodies: String::new(),
        tuple_names: HashSet::new(),
        helpers: HashSet::new(),
        queue: Vec::new(),
        seen: HashSet::new(),
        seen_consts: HashSet::new(),
        counter: 0,
        scopes: Vec::new(),
        loops: Vec::new(),
        cur_ret: Ty::unit(),
    };
    for item in &user.items {
        match &item.kind {
            ItemKind::Fn(f) => cg.register(f),
            ItemKind::Const(c) => {
                if let Some(id) = cg.res.const_decl.get(&NodeKey::of(c)) {
                    cg.consts.insert(*id, c);
                }
            }
            _ => {}
        }
    }
    let main = user.items.iter().find_map(|i| match &i.kind {
        ItemKind::Fn(f) if f.name.name == "main" => cg.res.fn_decl.get(&NodeKey::of(f)).copied(),
        _ => None,
    });
    let Some(main) = main else {
        return Err(NativeError::plain("the program has no `fn main()`"));
    };
    cg.request(main)?;
    while let Some(f) = cg.queue.pop() {
        cg.gen_fn(f)?;
    }
    let main_name = cg.fn_name(main);
    Ok(format!(
        "{}\n/* ===== generated from Tau ===== */\n{}\n{}\n{}\nstatic void tau_run_main(void) {{ {}(); }}\n",
        super::RUNTIME,
        cg.types,
        cg.protos,
        cg.bodies,
        main_name
    ))
}

impl<'a> Cg<'a> {
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
        self.res.expr_ty.get(&NodeKey::of(e)).cloned().unwrap_or(Ty::Error)
    }

    fn pat_ty(&self, p: &Pattern) -> Ty {
        self.res.pat_ty.get(&NodeKey::of(p)).cloned().unwrap_or(Ty::Error)
    }

    fn is_unit(t: &Ty) -> bool {
        t.is_unit()
    }

    fn fn_name(&self, f: FnId) -> String {
        format!("tau_fn_{}_{}", self.prog.fn_(f).name, f.0)
    }

    // ------------------------------------------------------------ types

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
            Ty::Param(_) => "generic code (milestone 3)".to_string(),
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
                let name = format!("tup_{}", self.mangle(ty)?);
                if self.tuple_names.insert(name.clone()) {
                    let mut fields = String::new();
                    for (i, t) in ts.iter().enumerate() {
                        let ct = self.cty(t)?;
                        fields.push_str(&format!(" {ct} f{i};"));
                    }
                    self.types.push_str(&format!("typedef struct {{{fields} }} {name};\n"));
                }
                name
            }
            other => return unsupported(self.describe(other)),
        })
    }

    /// Name used in the runtime's per-integer-type helpers (`tau_add_i64`, ...).
    fn int_suffix(it: IntTy) -> &'static str {
        it.name()
    }

    // ------------------------------------------------------------ functions

    fn request(&mut self, f: FnId) -> CgResult<()> {
        if self.seen.insert(f) {
            self.queue.push(f);
        }
        Ok(())
    }

    fn gen_fn(&mut self, id: FnId) -> CgResult<()> {
        let def = self.prog.fn_(id).clone();
        let Some(decl) = self.decls.get(&id).copied() else {
            return unsupported(format!("calling `{}`", def.name));
        };
        if !def.sig.generics.is_empty() || def.parent != Parent::Free {
            return unsupported(format!("`{}` (generic functions and methods, milestone 3)", def.name));
        }
        let Some(body) = &decl.body else {
            return unsupported(format!("calling `{}`", def.name));
        };
        let ret_c = self.cty(&def.sig.ret)?;
        let name = self.fn_name(id);
        self.scopes.push(Vec::new());
        self.loops.clear();
        self.cur_ret = def.sig.ret.clone();
        let mut params = Vec::new();
        for (p, pd) in decl.params.iter().filter(|p| p.name.name != "self").zip(&def.sig.params) {
            let ct = self.cty(&pd.ty)?;
            let cname = self.fresh(&format!("p_{}", p.name.name));
            let by_ptr = pd.mode == ParamMode::Mut;
            params.push(if by_ptr { format!("{ct} *{cname}") } else { format!("{ct} {cname}") });
            self.scopes.last_mut().unwrap().push(Local { name: p.name.name.clone(), cname, ty: pd.ty.clone(), by_ptr });
        }
        let plist = if params.is_empty() { "void".to_string() } else { params.join(", ") };
        let value = self.block_value(body)?;
        let body_ty = self.block_ty(body);
        let tail = if Self::is_unit(&def.sig.ret) || matches!(def.sig.ret, Ty::Never) {
            format!("(void)({value}); return 0;")
        } else if matches!(body_ty, Ty::Never) {
            format!("(void)({value}); tau_panic_cstr(\"unreachable\");")
        } else {
            format!("return {value};")
        };
        self.scopes.pop();
        self.protos.push_str(&format!("static {ret_c} {name}({plist});\n"));
        self.bodies.push_str(&format!("static {ret_c} {name}({plist}) {{\n  {tail}\n}}\n"));
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
        let l = Local { name: name.to_string(), cname, ty, by_ptr: false };
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
                    Stmt::Expr(e) => matches!(self.ty_of(e), Ty::Never),
                    Stmt::Let { init: Some(e), .. } => matches!(self.ty_of(e), Ty::Never),
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
        let mut tail: Option<String> = None;
        for (i, s) in b.stmts.iter().enumerate() {
            match s {
                Stmt::Let { pat, init, .. } => out.push_str(&self.let_stmt(pat, init.as_ref())?),
                Stmt::Expr(e) => {
                    let v = self.expr(e)?;
                    if i + 1 == n {
                        tail = Some(v);
                    } else {
                        out.push_str(&format!("(void)({v});\n"));
                    }
                }
                Stmt::Item(_) => {}
            }
        }
        self.scopes.pop();
        out.push_str(&tail.unwrap_or_else(|| "((tau_unit)0)".to_string()));
        out.push_str("; })");
        Ok(out)
    }

    fn let_stmt(&mut self, pat: &'a Pattern, init: Option<&'a Expr>) -> CgResult<String> {
        let ty = self.pat_ty(pat);
        let ct = self.cty(&ty)?;
        match init {
            Some(e) => {
                let v = self.expr(e)?;
                match &pat.kind {
                    PatKind::Ident { name, .. } => {
                        let l = self.declare(name, ty);
                        Ok(format!("{ct} {} = {v};\n", l.cname))
                    }
                    PatKind::Wild => Ok(format!("(void)({v});\n")),
                    _ => {
                        let tmp = self.fresh("let");
                        let mut s = format!("{ct} {tmp} = {v};\n");
                        self.bind_irrefutable(pat, &tmp, &ty, &mut s)?;
                        Ok(s)
                    }
                }
            }
            None => match &pat.kind {
                PatKind::Ident { name, .. } => {
                    let l = self.declare(name, ty);
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
                let l = self.declare(name, ty.clone());
                out.push_str(&format!("{ct} {} = {access};\n", l.cname));
                Ok(())
            }
            PatKind::Tuple(ps) => {
                let Ty::Tuple(ts) = ty else { return unsupported("this pattern") };
                for (i, (p, t)) in ps.iter().zip(ts).enumerate() {
                    self.bind_irrefutable(p, &format!("{access}.f{i}"), t, out)?;
                }
                Ok(())
            }
            _ => unsupported("this kind of pattern in `let`"),
        }
    }

    // ------------------------------------------------------------ expression helpers

    /// A place that can be assigned to or borrowed `mut`: its C lvalue text.
    fn place(&mut self, e: &'a Expr) -> CgResult<(String, Ty)> {
        match &e.kind {
            ExprKind::Ident(name) => {
                let Some(l) = self.lookup(name) else { return unsupported(format!("assigning to `{name}`")) };
                Ok((if l.by_ptr { format!("(*{})", l.cname) } else { l.cname.clone() }, l.ty))
            }
            ExprKind::Field { base, name } => {
                let (b, bt) = self.place(base)?;
                match (&bt, name.name.parse::<usize>()) {
                    (Ty::Tuple(ts), Ok(i)) if i < ts.len() => Ok((format!("{b}.f{i}"), ts[i].clone())),
                    _ => unsupported("assigning to this field"),
                }
            }
            _ => unsupported("assigning to this kind of place"),
        }
    }

    /// Evaluate operands left to right. Returns declarations to run first and the operand
    /// names to use afterwards. Literals and plain local reads are used in place.
    fn ordered(&mut self, es: &[&'a Expr]) -> CgResult<(String, Vec<String>)> {
        let mut exprs = Vec::new();
        for e in es {
            exprs.push(self.expr(e)?);
        }
        let trivial = |e: &Expr| matches!(e.kind, ExprKind::Int { .. } | ExprKind::Float { .. } | ExprKind::Bool(_) | ExprKind::Char(_) | ExprKind::Ident(_));
        let nontrivial = es.iter().filter(|e| !trivial(e)).count();
        let literals = es.iter().filter(|e| !matches!(e.kind, ExprKind::Int { .. } | ExprKind::Float { .. } | ExprKind::Bool(_) | ExprKind::Char(_))).count();
        if es.len() < 2 || nontrivial == 0 || literals < 2 {
            return Ok((String::new(), exprs));
        }
        let mut decls = String::new();
        let mut names = Vec::new();
        for (e, c) in es.iter().zip(exprs) {
            if matches!(e.kind, ExprKind::Int { .. } | ExprKind::Float { .. } | ExprKind::Bool(_) | ExprKind::Char(_)) {
                names.push(c);
            } else {
                let t = self.cty(&self.ty_of(e))?;
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

    /// A statement that appends `value` (of type `ty`) to the buffer `buf`.
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
            Ty::Tuple(_) => {
                let name = self.tuple_helper(ty, "fmt")?;
                format!("{name}({buf}, {value});")
            }
            other => return unsupported(format!("printing {}", self.describe(other))),
        })
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
            other => return unsupported(format!("comparing {}", self.describe(other))),
        })
    }

    /// -1, 0, 1, or 2 when the values are unordered (a NaN).
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
                let (decls, names) = self.ordered(&refs)?;
                let fields: Vec<String> = names.iter().map(|n| n.to_string()).collect();
                Ok(Self::wrap(decls, format!("(({ct}){{{}}})", fields.join(", "))))
            }
            ExprKind::Unary { op, expr } => self.unary(&ty, *op, expr),
            ExprKind::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs),
            ExprKind::Assign { op, lhs, rhs } => self.assign(*op, lhs, rhs),
            ExprKind::Cast { expr, .. } => {
                let from = self.ty_of(expr);
                let v = self.expr(expr)?;
                self.cast(&v, &from, &ty)
            }
            ExprKind::Call { callee, args } => self.call(e, callee, args),
            ExprKind::Field { base, name } => {
                let bt = self.ty_of(base);
                match (&bt, name.name.parse::<usize>()) {
                    (Ty::Tuple(ts), Ok(i)) if i < ts.len() => {
                        let b = self.expr(base)?;
                        Ok(format!("({b}).f{i}"))
                    }
                    _ => unsupported(format!("the field `{}`", name.name)),
                }
            }
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
                let val = match v {
                    Some(x) => {
                        let c = self.expr(x)?;
                        if Self::is_unit(&ret) || matches!(ret, Ty::Never) { format!("(void)({c}); return 0;") } else { format!("return {c};") }
                    }
                    None => "return 0;".to_string(),
                };
                Ok(format!("({{ {val} ((tau_unit)0); }})"))
            }
            ExprKind::Break(v) => {
                let Some(ctx) = self.loops.last() else { return unsupported("`break` here") };
                let (brk, result) = (ctx.brk.clone(), ctx.result.clone());
                let assign = match (v, result) {
                    (Some(x), Some(r)) => {
                        let c = self.expr(x)?;
                        format!("{r} = {c}; ")
                    }
                    (Some(x), None) => {
                        let c = self.expr(x)?;
                        format!("(void)({c}); ")
                    }
                    _ => String::new(),
                };
                Ok(format!("({{ {assign}goto {brk}; ((tau_unit)0); }})"))
            }
            ExprKind::Continue => {
                let Some(ctx) = self.loops.last() else { return unsupported("`continue` here") };
                Ok(format!("({{ goto {}; ((tau_unit)0); }})", ctx.cont))
            }
            ExprKind::Imag(_) => unsupported("complex numbers (milestone 4)"),
            ExprKind::Array(_) | ExprKind::ArrayRepeat { .. } => unsupported("arrays (milestone 2)"),
            ExprKind::Index { .. } => unsupported("indexing (milestone 2)"),
            ExprKind::MethodCall { .. } => unsupported("method calls (milestone 2)"),
            ExprKind::Closure { .. } => unsupported("closures (milestone 3)"),
            ExprKind::Range { .. } => unsupported("ranges outside a `for` loop (milestone 2)"),
            ExprKind::Try(_) => unsupported("the `?` operator (milestone 2)"),
            ExprKind::ParReduce { .. } | ExprKind::Spawn(_) | ExprKind::On { .. } => unsupported("parallel constructs (milestone 5)"),
            ExprKind::TypeApp { .. } | ExprKind::MutBorrow(_) => unsupported("this expression"),
        }
    }

    fn ident(&mut self, e: &'a Expr, name: &str) -> CgResult<String> {
        if let Some(l) = self.lookup(name) {
            return Ok(if l.by_ptr { format!("(*{})", l.cname) } else { l.cname });
        }
        match self.res.expr_res.get(&NodeKey::of(e)) {
            Some(Res::Const(cid)) => self.const_ref(*cid),
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
            let v = self.expr(&decl.value)?;
            self.scopes = saved_scopes;
            self.loops = saved_loops;
            self.protos.push_str(&format!("static {ct} {name}(void);\n"));
            self.bodies.push_str(&format!(
                "static {ct} {name}(void) {{ static bool done; static {ct} v; if (!done) {{ v = {v}; done = true; }} return v; }}\n"
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
        let mut s = format!("({{ tau_buf *{b} = (tau_buf *)calloc(1, sizeof(tau_buf)); ");
        s.push_str(&self.interp_into(&b, pieces)?);
        s.push_str(&format!("(tau_str){{ {b}->ptr, {b}->len }}; }})"));
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
                    let v = self.expr(inner)?;
                    let tmp = self.fresh("fv");
                    s.push_str(&format!("{{ {ct} {tmp} = {v}; "));
                    s.push_str(&self.fmt_stmt(buf, &tmp, &ty)?);
                    s.push_str(" }\n");
                }
            }
        }
        Ok(s)
    }

    // ------------------------------------------------------------ operators

    fn unary(&mut self, ty: &Ty, op: UnOp, inner: &'a Expr) -> CgResult<String> {
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

    fn binary(&mut self, op: BinOp, lhs: &'a Expr, rhs: &'a Expr) -> CgResult<String> {
        if self.res.expr_res.contains_key(&NodeKey::of(lhs)) && false {
            return unsupported("operator overloading");
        }
        let lt = self.ty_of(lhs);
        if matches!(op, BinOp::And | BinOp::Or) {
            let (l, r) = (self.expr(lhs)?, self.expr(rhs)?);
            return Ok(format!("({l} {} {r})", if op == BinOp::And { "&&" } else { "||" }));
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

    fn assign(&mut self, op: Option<BinOp>, lhs: &'a Expr, rhs: &'a Expr) -> CgResult<String> {
        let lt = self.ty_of(lhs);
        let (target, _) = self.place(lhs)?;
        let r = self.expr(rhs)?;
        let rt = self.cty(&self.ty_of(rhs))?;
        let rv = self.fresh("rv");
        Ok(match op {
            None => format!("({{ {rt} {rv} = {r}; {target} = {rv}; ((tau_unit)0); }})"),
            Some(bop) => {
                let value = self.arith(bop, &target, &rv, &lt)?;
                format!("({{ {rt} {rv} = {r}; {target} = {value}; ((tau_unit)0); }})")
            }
        })
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

    fn call(&mut self, e: &'a Expr, callee: &'a Expr, args: &'a [Expr]) -> CgResult<String> {
        let Some(Res::Fn(fid, subst)) = self.res.expr_res.get(&NodeKey::of(callee)) else {
            return unsupported("this kind of call");
        };
        let (fid, subst) = (*fid, subst.clone());
        let def = self.prog.fn_(fid).clone();
        if self.decls.contains_key(&fid) {
            if !def.sig.generics.is_empty() {
                return unsupported(format!("calling the generic function `{}` (milestone 3)", def.name));
            }
            self.request(fid)?;
            let name = self.fn_name(fid);
            let refs: Vec<&'a Expr> = args.iter().collect();
            let (decls, names) = self.ordered_args(&refs, &def.sig)?;
            return Ok(Self::wrap(decls, format!("{name}({})", names.join(", "))));
        }
        self.builtin(e, &def, &subst, args)
    }

    /// Arguments in order; `mut` arguments become pointers to their place.
    fn ordered_args(&mut self, args: &[&'a Expr], sig: &FnSig) -> CgResult<(String, Vec<String>)> {
        let mut decls = String::new();
        let mut names = Vec::new();
        let temps = args.len() >= 2 && args.iter().filter(|a| !matches!(a.kind, ExprKind::Int { .. } | ExprKind::Float { .. } | ExprKind::Bool(_) | ExprKind::Char(_))).count() >= 2;
        for (i, a) in args.iter().enumerate() {
            let mode = sig.params.get(i).map(|p| p.mode).unwrap_or(ParamMode::Borrow);
            if mode == ParamMode::Mut {
                let inner: &'a Expr = match &a.kind {
                    ExprKind::MutBorrow(x) => x,
                    _ => a,
                };
                let (place, _) = self.place(inner)?;
                // a `mut` parameter is already a pointer: hand it on as is
                let ptr = match (&inner.kind, place.strip_prefix("(*").and_then(|p| p.strip_suffix(')'))) {
                    (ExprKind::Ident(_), Some(p)) => p.to_string(),
                    _ => format!("&{place}"),
                };
                names.push(ptr);
                continue;
            }
            let v = self.expr(a)?;
            if temps && !matches!(a.kind, ExprKind::Int { .. } | ExprKind::Float { .. } | ExprKind::Bool(_) | ExprKind::Char(_)) {
                let t = self.cty(&self.ty_of(a))?;
                let tmp = self.fresh("arg");
                decls.push_str(&format!("{t} {tmp} = {v}; "));
                names.push(tmp);
            } else {
                names.push(v);
            }
        }
        Ok((decls, names))
    }

    fn builtin(&mut self, e: &'a Expr, def: &FnDef, subst: &[(GenericId, Ty)], args: &'a [Expr]) -> CgResult<String> {
        if def.module != 0 {
            return unsupported(format!("the library function `{}`", def.name));
        }
        let targ = subst.first().map(|(_, t)| t.clone());
        let name = def.name.as_str();
        match name {
            "print" => {
                let a = &args[0];
                let ty = self.ty_of(a);
                if let ExprKind::Str(pieces) = &a.kind {
                    if pieces.iter().any(|p| matches!(p, StrPiece::Interp(_))) {
                        let body = self.interp_into("o_", pieces)?;
                        return Ok(format!("({{ tau_buf *o_ = tau_out_begin(); {body}tau_buf_char(o_, '\\n'); tau_out_end(); ((tau_unit)0); }})"));
                    }
                }
                let ct = self.cty(&ty)?;
                let v = self.expr(a)?;
                let f = self.fmt_stmt("o_", "pv_", &ty)?;
                Ok(format!("({{ {ct} pv_ = {v}; tau_buf *o_ = tau_out_begin(); {f} tau_buf_char(o_, '\\n'); tau_out_end(); ((tau_unit)0); }})"))
            }
            "assert" => {
                let c = self.expr(&args[0])?;
                Ok(format!("({{ if (!({c})) tau_panic_cstr(\"assertion failed\"); ((tau_unit)0); }})"))
            }
            "assert_eq" => {
                let ty = self.ty_of(&args[0]);
                let ct = self.cty(&ty)?;
                let (decls, n) = self.ordered(&[&args[0], &args[1]])?;
                let eq = self.eq_expr("ea_", "eb_", &ty)?;
                let fa = self.fmt_stmt("&m_", "ea_", &ty)?;
                let fb = self.fmt_stmt("&m_", "eb_", &ty)?;
                Ok(format!(
                    "({{ {decls}{ct} ea_ = {}; {ct} eb_ = {}; if (!{eq}) {{ tau_buf m_ = {{0}}; tau_buf_cstr(&m_, \"assertion failed: left != right\\n  left: \"); {fa} tau_buf_cstr(&m_, \"\\n right: \"); {fb} tau_panic((const char *)m_.ptr, m_.len); }} ((tau_unit)0); }})",
                    n[0], n[1]
                ))
            }
            "panic" => {
                let m = self.expr(&args[0])?;
                Ok(format!("({{ tau_panic_str({m}); ((tau_unit)0); }})"))
            }
            "sqrt" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "exp" | "ln" | "log10" | "floor" | "ceil" | "round" => {
                let single = matches!(targ, Some(Ty::Float(FloatTy::F32)));
                let f = match name {
                    "ln" => "log",
                    n => n,
                };
                let x = self.expr(&args[0])?;
                Ok(if single { format!("((float){f}((double)({x})))") } else { format!("{f}({x})") })
            }
            "atan2" => {
                let single = matches!(targ, Some(Ty::Float(FloatTy::F32)));
                let (decls, n) = self.ordered(&[&args[0], &args[1]])?;
                let v = if single { format!("((float)atan2((double)({}), (double)({})))", n[0], n[1]) } else { format!("atan2({}, {})", n[0], n[1]) };
                Ok(Self::wrap(decls, v))
            }
            "abs" => {
                let x = self.expr(&args[0])?;
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
                let (decls, n) = self.ordered(&[&args[0], &args[1]])?;
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
            other => {
                let _ = e;
                unsupported(format!("the library function `{other}`"))
            }
        }
    }

    // ------------------------------------------------------------ control flow

    fn if_expr(&mut self, ty: &Ty, cond: &'a Expr, then: &'a Block, else_: Option<&'a Expr>) -> CgResult<String> {
        let c = self.expr(cond)?;
        let never = matches!(ty, Ty::Never);
        let ct = self.cty(ty)?;
        let r = self.fresh("if");
        let then_never = matches!(self.block_ty(then), Ty::Never);
        let t = self.block_value(then)?;
        let then_stmt = if never || then_never { format!("(void)({t});") } else { format!("{r} = {t};") };
        let mut s = format!("({{ {ct} {r} = ({ct}){{0}}; if ({c}) {{ {then_stmt} }}");
        if let Some(el) = else_ {
            let else_never = matches!(self.ty_of(el), Ty::Never);
            let ev = self.expr(el)?;
            let else_stmt = if never || else_never { format!("(void)({ev});") } else { format!("{r} = {ev};") };
            s.push_str(&format!(" else {{ {else_stmt} }}"));
        }
        s.push_str(&format!(" {r}; }})"));
        Ok(s)
    }

    fn pat_has_bindings(&self, p: &Pattern) -> bool {
        match &p.kind {
            PatKind::Ident { .. } => true,
            PatKind::Tuple(ps) => ps.iter().any(|x| self.pat_has_bindings(x)),
            PatKind::Or(alts) => alts.iter().any(|x| self.pat_has_bindings(x)),
            _ => false,
        }
    }

    /// A C condition that is true when `access` (of type `ty`) matches `p`.
    fn pat_test(&mut self, p: &'a Pattern, access: &str, ty: &Ty) -> CgResult<String> {
        Ok(match &p.kind {
            PatKind::Wild | PatKind::Ident { .. } => "1".to_string(),
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
                if self.pat_has_bindings(p) {
                    return unsupported("`|` patterns that bind variables");
                }
                let mut parts = Vec::new();
                for a in alts {
                    parts.push(self.pat_test(a, access, ty)?);
                }
                format!("({})", parts.join(" || "))
            }
            PatKind::Variant { .. } | PatKind::Path(_) => return unsupported("enum patterns (milestone 2)"),
        })
    }

    fn pat_bind(&mut self, p: &'a Pattern, access: &str, ty: &Ty, out: &mut String) -> CgResult<()> {
        match &p.kind {
            PatKind::Ident { name, .. } => {
                let ct = self.cty(ty)?;
                let l = self.declare(name, ty.clone());
                out.push_str(&format!("{ct} {} = {access}; ", l.cname));
            }
            PatKind::Tuple(ps) => {
                let Ty::Tuple(ts) = ty else { return unsupported("this pattern") };
                for (i, (sub, t)) in ps.iter().zip(ts).enumerate() {
                    self.pat_bind(sub, &format!("{access}.f{i}"), t, out)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn match_expr(&mut self, ty: &Ty, scrutinee: &'a Expr, arms: &'a [MatchArm]) -> CgResult<String> {
        let st = self.ty_of(scrutinee);
        let sct = self.cty(&st)?;
        let sv = self.expr(scrutinee)?;
        let never = matches!(ty, Ty::Never);
        let ct = self.cty(ty)?;
        let (s, r, end) = (self.fresh("scrut"), self.fresh("m"), self.fresh("mend"));
        let mut out = format!("({{ {sct} {s} = {sv}; {ct} {r} = ({ct}){{0}};\n");
        for arm in arms {
            self.scopes.push(Vec::new());
            let test = self.pat_test(&arm.pat, &s, &st)?;
            let mut binds = String::new();
            self.pat_bind(&arm.pat, &s, &st, &mut binds)?;
            let guard = match &arm.guard {
                Some(g) => Some(self.expr(g)?),
                None => None,
            };
            let body_never = matches!(self.ty_of(&arm.body), Ty::Never);
            let body = self.expr(&arm.body)?;
            let assign = if never || body_never { format!("(void)({body});") } else { format!("{r} = {body};") };
            let finish = format!("{assign} goto {end};");
            let inner = match guard {
                Some(g) => format!("if ({g}) {{ {finish} }}"),
                None => finish,
            };
            out.push_str(&format!("if ({test}) {{ {binds}{inner} }}\n"));
            self.scopes.pop();
        }
        out.push_str(&format!("tau_panic_cstr(\"no match arm matched the value\");\n{end}: ; {r}; }})"));
        Ok(out)
    }

    fn loop_expr(&mut self, ty: &Ty, body: &'a Block) -> CgResult<String> {
        let (brk, cont) = (self.fresh("brk"), self.fresh("cont"));
        let never = matches!(ty, Ty::Never);
        let (result, decl) = if never || Self::is_unit(ty) {
            (None, String::new())
        } else {
            let ct = self.cty(ty)?;
            let r = self.fresh("lv");
            (Some(r.clone()), format!("{ct} {r} = ({ct}){{0}}; "))
        };
        self.loops.push(LoopCtx { brk: brk.clone(), cont: cont.clone(), result: result.clone() });
        let b = self.block_value(body);
        self.loops.pop();
        let b = b?;
        let value = result.unwrap_or_else(|| "((tau_unit)0)".to_string());
        Ok(format!("({{ {decl}for (;;) {{ (void)({b}); {cont}: ; }} {brk}: ; {value}; }})"))
    }

    fn while_expr(&mut self, cond: &'a Expr, body: &'a Block) -> CgResult<String> {
        let (brk, cont) = (self.fresh("brk"), self.fresh("cont"));
        self.loops.push(LoopCtx { brk: brk.clone(), cont: cont.clone(), result: None });
        let c = self.expr(cond);
        let b = self.block_value(body);
        self.loops.pop();
        let (c, b) = (c?, b?);
        Ok(format!("({{ for (;;) {{ if (!({c})) goto {brk}; (void)({b}); {cont}: ; }} {brk}: ; ((tau_unit)0); }})"))
    }

    fn for_expr(&mut self, pat: &'a Pattern, iter: &'a Expr, guard: Option<&'a Expr>, body: &'a Block) -> CgResult<String> {
        let ExprKind::Range { start: Some(start), end: Some(end), inclusive } = &iter.kind else {
            return unsupported("`for` over anything but a range (milestone 2)");
        };
        let ty = self.ty_of(start);
        let Ty::Int(_) = ty else { return unsupported("a `for` range over non-integers") };
        let ct = self.cty(&ty)?;
        let (decls, n) = self.ordered(&[start, end])?;
        let (lo, hi, i) = (self.fresh("lo"), self.fresh("hi"), self.fresh("i"));
        let (brk, cont) = (self.fresh("brk"), self.fresh("cont"));
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
        self.loops.push(LoopCtx { brk: brk.clone(), cont: cont.clone(), result: None });
        let g = match guard {
            Some(g) => self.expr(g).map(|c| format!("if (!({c})) goto {cont}; ")),
            None => Ok(String::new()),
        };
        let b = self.block_value(body);
        self.loops.pop();
        self.scopes.pop();
        let (g, b) = (g?, b?);
        let head = if *inclusive {
            format!("if ({lo} <= {hi}) for ({ct} {i} = {lo};; {i}++)")
        } else {
            format!("for ({ct} {i} = {lo}; {i} < {hi}; {i}++)")
        };
        let tail = if *inclusive { format!("if ({i} == {hi}) goto {brk};") } else { String::new() };
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
