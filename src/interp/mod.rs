//! A tree-walking interpreter over the checked AST.
//!
//! It relies on the type checker's tables (what each name refers to, the static type of
//! every expression) and on the borrow checker having accepted the program. `par`
//! loops, `spawn` and `on` run sequentially: the results are the same, just not faster.

mod builtins;
mod eval;
mod tensor;
pub mod value;

use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;

use crate::ast::*;
use crate::check::{CheckResult, NodeKey, Res, Results};
use crate::program::*;
use crate::types::*;
use crate::types::Dim;
use value::*;

/// Non-local control flow.
pub enum Ctl<'a> {
    Break(Option<Value<'a>>),
    Continue,
    Return(Value<'a>),
    Panic(String),
}

pub type R<'a, T> = Result<T, Ctl<'a>>;

pub fn panic<'a, T>(msg: impl Into<String>) -> R<'a, T> {
    Err(Ctl::Panic(msg.into()))
}

pub struct Frame<'a> {
    pub env: Rc<Env<'a>>,
    /// What the generic parameters of the running function stand for.
    pub tyargs: Subst,
}

impl<'a> Frame<'a> {
    fn child(&self) -> Frame<'a> {
        Frame { env: Env::child(&self.env), tyargs: self.tyargs.clone() }
    }
}

/// How a run ended.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Ok,
    /// The program panicked (message).
    Panic(String),
    /// `main` returned an `Err` (its rendering).
    Error(String),
}

pub struct Interp<'a> {
    pub prog: &'a Program,
    pub res: &'a Results,
    pub mod_paths: Vec<String>,
    pub fns: Vec<Option<&'a FnDecl>>,
    pub consts: HashMap<ConstId, &'a ConstDecl>,
    const_cache: HashMap<ConstId, Value<'a>>,
    /// Built-in dispatch keys, computed once per function.
    key_cache: Vec<Option<Rc<str>>>,
    pub out: Box<dyn Write>,
    pub rng: u64,
    /// Address near the top of the stack when the program started, and how many bytes of
    /// stack the program may use before it is stopped with a panic.
    stack_base: usize,
    stack_limit: usize,
    /// Number of expressions evaluated so far, and the most the program may evaluate.
    pub steps: u64,
    pub max_steps: u64,
    pub option: AdtId,
    pub result: AdtId,
    pub range_adt: Option<AdtId>,
}


fn make_interp<'a>(
    user: &'a Module,
    check: &'a CheckResult,
    lib_paths: Vec<String>,
    out: Box<dyn Write>,
    stack_base: usize,
    stack_bytes: usize,
    max_steps: u64,
) -> Interp<'a> {
    let mut mod_paths = lib_paths;
    mod_paths.push("main".to_string());
    let prog = &check.program;
    let res = &check.results;
    let mut it = Interp {
        prog,
        res,
        mod_paths,
        fns: vec![None; prog.fns.len()],
        consts: HashMap::new(),
        const_cache: HashMap::new(),
        key_cache: vec![None; prog.fns.len()],
        out,
        rng: 0x9E37_79B9_7F4A_7C15,
        stack_base,
        stack_limit: stack_bytes / 10 * 9,
        steps: 0,
        max_steps,
        option: prog.find_adt("Option").expect("prelude defines Option"),
        result: prog.find_adt("Result").expect("prelude defines Result"),
        range_adt: prog.find_adt("Range"),
    };
    it.collect(&user.items);
    it
}

/// Run `main` in the user's module. The program must have passed the type checker and
/// the borrow checker.
pub fn run_main(user: &Module, check: &CheckResult, lib_paths: Vec<String>, out: Box<dyn Write>, stack_bytes: usize, max_steps: u64) -> Outcome {
    let marker = 0u8;
    let mut it = make_interp(user, check, lib_paths, out, &marker as *const u8 as usize, stack_bytes, max_steps);
    let res = &check.results;
    let main = user.items.iter().find_map(|i| match &i.kind {
        ItemKind::Fn(f) if f.name.name == "main" => res.fn_decl.get(&NodeKey::of(f)).copied(),
        _ => None,
    });
    let Some(main) = main else {
        return Outcome::Error("the program has no `fn main()`".to_string());
    };
    match it.call_fn(main, Vec::new(), Vec::new()) {
        Ok(v) => {
            if let Value::Variant(adt, 1, payload) = &v {
                if *adt == it.result {
                    let e = payload.borrow()[0].clone();
                    let text = it.display(&e).unwrap_or_else(|_| "<error>".to_string());
                    let _ = it.out.flush();
                    return Outcome::Error(text);
                }
            }
            let _ = it.out.flush();
            Outcome::Ok
        }
        Err(Ctl::Panic(m)) => {
            let _ = it.out.flush();
            Outcome::Panic(m)
        }
        Err(_) => {
            let _ = it.out.flush();
            Outcome::Ok
        }
    }
}

/// How one `#[test]` function ended.
#[derive(Debug, Clone, PartialEq)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    /// Why it failed (empty when it passed).
    pub message: String,
    /// What the test printed.
    pub output: String,
}

/// A `Write` whose contents can be read back: each test's output is captured.
#[derive(Clone, Default)]
struct SharedBuf(Rc<std::cell::RefCell<Vec<u8>>>);

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The text of a string literal argument: `#[should_panic("text")]`.
fn attr_text(a: &Attribute) -> Option<String> {
    match a.args.first().map(|e| &e.kind) {
        Some(ExprKind::Str(pieces)) => Some(
            pieces
                .iter()
                .filter_map(|p| if let StrPiece::Lit(l) = p { Some(l.as_str()) } else { None })
                .collect(),
        ),
        _ => None,
    }
}

/// Run every `#[test]` function in the user's module whose name contains `filter`.
/// Each test runs in a fresh scope; a panic (or an `Err` result) fails it. A test marked
/// `#[should_panic]` passes only if it panics, and `#[should_panic("text")]` also needs
/// the message to contain `text`.
pub fn run_tests(
    user: &Module,
    check: &CheckResult,
    lib_paths: Vec<String>,
    stack_bytes: usize,
    max_steps: u64,
    filter: Option<&str>,
) -> Vec<TestResult> {
    let marker = 0u8;
    let mut it = make_interp(user, check, lib_paths, Box::new(std::io::sink()), &marker as *const u8 as usize, stack_bytes, max_steps);
    let mut results = Vec::new();
    for item in &user.items {
        let ItemKind::Fn(f) = &item.kind else { continue };
        if !item.attrs.iter().any(|a| a.name.name == "test") {
            continue;
        }
        if let Some(pat) = filter {
            if !f.name.name.contains(pat) {
                continue;
            }
        }
        let Some(fid) = check.results.fn_decl.get(&NodeKey::of(f)).copied() else { continue };
        let should_panic = item.attrs.iter().find(|a| a.name.name == "should_panic");
        let buf = SharedBuf::default();
        it.out = Box::new(buf.clone());
        it.steps = 0;
        let outcome = it.call_fn(fid, Vec::new(), Vec::new());
        let output = String::from_utf8_lossy(&buf.0.borrow()).to_string();
        let (passed, message) = match (outcome, should_panic) {
            (Ok(v), None) => match &v {
                Value::Variant(adt, 1, payload) if *adt == it.result => {
                    let e = payload.borrow()[0].clone();
                    let text = it.display(&e).unwrap_or_else(|_| "<error>".to_string());
                    (false, format!("returned an error: {text}"))
                }
                _ => (true, String::new()),
            },
            (Ok(_), Some(_)) => (false, "expected the test to panic, but it did not".to_string()),
            (Err(Ctl::Panic(m)), None) => (false, format!("panicked: {m}")),
            (Err(Ctl::Panic(m)), Some(a)) => match attr_text(a) {
                Some(want) if !m.contains(&want) => (false, format!("panicked with {m:?}, which does not contain {want:?}")),
                _ => (true, String::new()),
            },
            (Err(_), _) => (true, String::new()),
        };
        results.push(TestResult { name: f.name.name.clone(), passed, message, output });
    }
    results
}

impl<'a> Interp<'a> {
    // ------------------------------------------------------------ setup

    fn collect(&mut self, items: &'a [Item]) {
        for item in items {
            match &item.kind {
                ItemKind::Fn(f) => self.register_fn(f),
                ItemKind::Impl(i) => {
                    for it in &i.items {
                        if let ItemKind::Fn(f) = &it.kind {
                            self.register_fn(f);
                        }
                    }
                }
                ItemKind::Trait(t) => {
                    for it in &t.items {
                        if let ItemKind::Fn(f) = &it.kind {
                            self.register_fn(f);
                        }
                    }
                }
                ItemKind::Const(c) => {
                    if let Some(id) = self.res.const_decl.get(&NodeKey::of(c)) {
                        self.consts.insert(*id, c);
                    }
                }
                _ => {}
            }
        }
    }

    fn register_fn(&mut self, f: &'a FnDecl) {
        if let Some(id) = self.res.fn_decl.get(&NodeKey::of(f)) {
            self.fns[id.idx()] = Some(f);
        }
    }

    // ------------------------------------------------------------ types

    pub fn ty_of(&self, e: &Expr, f: &Frame<'a>) -> Ty {
        match self.res.expr_ty.get(&NodeKey::of(e)) {
            Some(t) => t.subst(&f.tyargs),
            None => Ty::Error,
        }
    }

    pub fn pat_ty(&self, p: &Pattern, f: &Frame<'a>) -> Ty {
        match self.res.pat_ty.get(&NodeKey::of(p)) {
            Some(t) => t.subst(&f.tyargs),
            None => Ty::Error,
        }
    }

    // ------------------------------------------------------------ references

    pub fn load(&self, t: &RefTarget<'a>) -> R<'a, Value<'a>> {
        Ok(match t {
            RefTarget::Var(env, i) => env.vars.borrow()[*i].1.clone(),
            RefTarget::Slot(s, i) => s.borrow()[*i].clone(),
            RefTarget::MapEntry(m, k) => {
                let m = m.borrow();
                match m.index.get(k) {
                    Some(i) => m.entries[*i].1.clone(),
                    None => return panic("key not found in map"),
                }
            }
            RefTarget::TensorElem(t, i) => {
                let t = t.borrow();
                match &t.data {
                    TData::F(d) => Value::Float(d[*i]),
                    TData::C(d) => Value::Complex(d[*i].0, d[*i].1),
                }
            }
        })
    }

    pub fn store(&self, t: &RefTarget<'a>, v: Value<'a>) -> R<'a, ()> {
        match t {
            RefTarget::Var(env, i) => env.vars.borrow_mut()[*i].1 = v,
            RefTarget::Slot(s, i) => s.borrow_mut()[*i] = v,
            RefTarget::MapEntry(m, k) => {
                let mut m = m.borrow_mut();
                match m.index.get(k).copied() {
                    Some(i) => m.entries[i].1 = v,
                    None => return panic("key not found in map"),
                }
            }
            RefTarget::TensorElem(t, i) => {
                let mut t = t.borrow_mut();
                let single = t.single;
                match (&mut t.data, v) {
                    (TData::F(d), Value::Float(x)) => d[*i] = if single { x as f32 as f64 } else { x },
                    (TData::F(d), Value::Int(x)) => d[*i] = x as f64,
                    (TData::C(d), Value::Complex(a, b)) => d[*i] = (a, b),
                    _ => return panic("cannot store this value in a tensor"),
                }
            }
        }
        Ok(())
    }

    /// Follow references until a plain value is reached.
    pub fn deref(&self, v: Value<'a>) -> R<'a, Value<'a>> {
        let mut cur = v;
        while let Value::Ref(t) = &cur {
            let next = self.load(t)?;
            cur = next;
        }
        Ok(cur)
    }

    // ------------------------------------------------------------ calls

    fn substitute(&self, subst: &[(GenericId, Ty)], f: &Frame<'a>) -> Vec<(GenericId, Ty)> {
        subst.iter().map(|(g, t)| (*g, t.subst(&f.tyargs))).collect()
    }

    /// The key identifying a built-in function: `"sqrt"`, `"Vec::push"`, `"std.fs::read"`.
    fn builtin_key(&self, fid: FnId) -> String {
        let def = self.prog.fn_(fid);
        match def.parent {
            Parent::Impl(i) => {
                let head = match &self.prog.impl_(i).self_ty {
                    Ty::Adt(a, _) => self.prog.adt(*a).name.clone(),
                    Ty::Slice(_) => "slice".to_string(),
                    Ty::Str => "str".to_string(),
                    _ => "?".to_string(),
                };
                format!("{head}::{}", def.name)
            }
            _ => {
                let path = &self.mod_paths[def.module];
                if path == "prelude" {
                    def.name.clone()
                } else {
                    format!("{path}::{}", def.name)
                }
            }
        }
    }

    /// If `fid` is a trait method and `Self` is a known type, find the impl's version.
    fn resolve_trait_method(&self, fid: FnId, subst: &[(GenericId, Ty)]) -> Option<(FnId, Vec<(GenericId, Ty)>)> {
        let def = self.prog.fn_(fid);
        let Parent::Trait(tid) = def.parent else { return None };
        let tdef = self.prog.trait_(tid);
        let self_ty = subst.iter().find(|(g, _)| *g == tdef.self_generic).map(|(_, t)| t.clone())?;
        if matches!(self_ty, Ty::Param(_) | Ty::Var(_) | Ty::Error) {
            return None;
        }
        for imp in &self.prog.impls {
            if imp.trait_ref.as_ref().map(|t| t.trait_) != Some(tid) {
                continue;
            }
            let mut binds: Vec<(GenericId, Ty)> = Vec::new();
            if !match_ty(&imp.self_ty, &self_ty, &mut binds) {
                continue;
            }
            let Some((_, ifid)) = imp.methods.iter().find(|(n, _)| *n == def.name) else {
                // a default method: runs with `Self` bound
                return Some((fid, subst.to_vec()));
            };
            // the method's own generics follow the impl's
            let isig = &self.prog.fn_(*ifid).sig;
            for (tg, ig) in def.sig.generics.iter().zip(&isig.generics) {
                if let Some((_, t)) = subst.iter().find(|(g, _)| g == tg) {
                    binds.push((*ig, t.clone()));
                }
            }
            return Some((*ifid, binds));
        }
        None
    }

    pub fn call_fn(&mut self, fid: FnId, subst: Vec<(GenericId, Ty)>, args: Vec<Value<'a>>) -> R<'a, Value<'a>> {
        let (fid, subst) = match self.resolve_trait_method(fid, &subst) {
            Some(r) => r,
            None => (fid, subst),
        };
        let def = self.prog.fn_(fid);
        let Some(decl) = self.fns[fid.idx()] else {
            return self.call_builtin_fn(fid, args, subst);
        };
        let Some(body) = &decl.body else {
            return self.call_builtin_fn(fid, args, subst);
        };
        self.check_stack()?;
        let env = Env::root();
        let mut it = args.into_iter();
        for p in &decl.params {
            let Some(v) = it.next() else { break };
            env.declare(&p.name.name, v);
        }
        let frame = Frame { env, tyargs: Rc::new(subst) };
        let r = self.eval_block(body, &frame);
        match r {
            Ok(v) => Ok(v),
            Err(Ctl::Return(v)) => Ok(v),
            Err(Ctl::Break(_)) | Err(Ctl::Continue) => panic("`break` or `continue` outside of a loop"),
            Err(p) => Err(p),
        }
    }

    fn call_builtin_fn(&mut self, fid: FnId, args: Vec<Value<'a>>, subst: Vec<(GenericId, Ty)>) -> R<'a, Value<'a>> {
        let key = match &self.key_cache[fid.idx()] {
            Some(k) => k.clone(),
            None => {
                let k: Rc<str> = Rc::from(self.builtin_key(fid));
                self.key_cache[fid.idx()] = Some(k.clone());
                k
            }
        };
        let ret = self.prog.fn_(fid).sig.ret.subst(&subst);
        self.call_builtin(&key, args, &ret, &subst)
    }

    /// Stop the program with a panic, rather than crash, when the stack is nearly full.
    fn check_stack(&self) -> R<'a, ()> {
        let marker = 0u8;
        let here = &marker as *const u8 as usize;
        if self.stack_base.saturating_sub(here) > self.stack_limit {
            return panic("stack overflow: the program recursed too deeply");
        }
        Ok(())
    }

    pub fn call_value(&mut self, fv: Value<'a>, args: Vec<Value<'a>>) -> R<'a, Value<'a>> {
        let fv = self.deref(fv)?;
        match fv {
            Value::Closure(c) => {
                self.check_stack()?;
                let frame = Frame { env: Env::child(&c.env), tyargs: c.tyargs.clone() };
                for (p, a) in c.params.iter().zip(args) {
                    if !self.bind(&p.pat, &a, &frame, &frame.env)? {
                        return panic("closure parameter pattern did not match");
                    }
                }
                let r = self.eval_val(c.body, &frame);
                match r {
                    Err(Ctl::Return(v)) => Ok(v),
                    other => other,
                }
            }
            Value::Func(fid, subst) => self.call_fn(fid, subst.to_vec(), args),
            Value::Ctor(adt, variant) => Ok(match variant {
                Some(i) => Value::variant(adt, i, args),
                None => Value::Struct(adt, Value::slot(args)),
            }),
            other => panic(format!("cannot call a value of type {}", other.type_name())),
        }
    }

    // ------------------------------------------------------------ constants

    pub fn const_value(&mut self, cid: ConstId) -> R<'a, Value<'a>> {
        if let Some(v) = self.const_cache.get(&cid) {
            return Ok(v.clone());
        }
        let def = self.prog.consts[cid.idx()].clone();
        let v = if def.module == 0 {
            match def.name.as_str() {
                "PI" => Value::Float(std::f64::consts::PI),
                "TAU" => Value::Float(std::f64::consts::TAU),
                "E" => Value::Float(std::f64::consts::E),
                other => return panic(format!("unknown built-in constant `{other}`")),
            }
        } else if let Some(decl) = self.consts.get(&cid).copied() {
            let frame = Frame { env: Env::root(), tyargs: Rc::new(Vec::new()) };
            self.eval_val(&decl.value, &frame)?
        } else {
            // a `cluster` declaration: a handle with no behaviour yet
            Value::Unit
        };
        self.const_cache.insert(cid, v.clone());
        Ok(v)
    }

    // ------------------------------------------------------------ display

    /// The text of a float: the shortest digits that read back exactly, never in
    /// scientific notation, and with `.0` added when the value is a whole number.
    pub fn float_text(&self, x: f64) -> String {
        if x.is_nan() {
            "NaN".to_string()
        } else if x.is_infinite() {
            if x > 0.0 { "inf".to_string() } else { "-inf".to_string() }
        } else {
            let mut s = format!("{x}");
            if !s.contains('.') {
                s.push_str(".0");
            }
            s
        }
    }

    /// Like `float_text`, for a single-precision value: the shortest digits that read back
    /// exactly *as an `f32`* (so `0.1f32` prints as `0.1`, not as its `f64` widening).
    pub fn f32_text(&self, x: f32) -> String {
        if x.is_nan() {
            "NaN".to_string()
        } else if x.is_infinite() {
            if x > 0.0 { "inf".to_string() } else { "-inf".to_string() }
        } else {
            let mut s = format!("{x}");
            if !s.contains('.') {
                s.push_str(".0");
            }
            s
        }
    }

    /// Display a value whose static type is known, which lets `f32`s (and containers of
    /// them) print in single precision. Falls back to `display` for everything else.
    pub fn display_ty(&mut self, v: &Value<'a>, ty: &Ty) -> R<'a, String> {
        let v = self.deref(v.clone())?;
        match (&v, ty) {
            (Value::Float(x), Ty::Float(FloatTy::F32)) => Ok(self.f32_text(*x as f32)),
            (Value::Complex(a, b), Ty::Complex(FloatTy::F32)) => {
                let (re, im) = (self.f32_text(*a as f32), self.f32_text(b.abs() as f32));
                Ok(if *b < 0.0 { format!("{re}-{im}i") } else { format!("{re}+{im}i") })
            }
            (Value::Tuple(s), Ty::Tuple(ts)) if s.borrow().len() == ts.len() => {
                let items = s.borrow().clone();
                let mut parts = Vec::new();
                for (x, t) in items.iter().zip(ts) {
                    parts.push(self.display_ty(x, t)?);
                }
                Ok(if parts.len() == 1 { format!("({},)", parts[0]) } else { format!("({})", parts.join(", ")) })
            }
            (Value::Array(s) | Value::Vec(s), t) => {
                let elem = match t {
                    Ty::Array(e, _) | Ty::Slice(e) => Some((**e).clone()),
                    Ty::Adt(_, args) if args.len() == 1 => Some(args[0].clone()),
                    _ => None,
                };
                let Some(elem) = elem else { return self.display(&v) };
                let items = s.borrow().clone();
                let mut parts = Vec::new();
                for x in &items {
                    parts.push(self.display_ty(x, &elem)?);
                }
                Ok(format!("[{}]", parts.join(", ")))
            }
            (Value::Slice(s, a, b), Ty::Slice(elem) | Ty::Array(elem, _)) => {
                let items: Vec<Value<'a>> = s.borrow()[*a..*b].to_vec();
                let mut parts = Vec::new();
                for x in &items {
                    parts.push(self.display_ty(x, elem)?);
                }
                Ok(format!("[{}]", parts.join(", ")))
            }
            _ => self.display(&v),
        }
    }

    pub fn display(&mut self, v: &Value<'a>) -> R<'a, String> {
        let v = self.deref(v.clone())?;
        Ok(match &v {
            Value::Unit => "()".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(x) => self.float_text(*x),
            Value::Complex(a, b) => {
                if *b < 0.0 || (*b == 0.0 && b.is_sign_negative()) {
                    format!("{}-{}i", self.float_text(*a), self.float_text(-*b))
                } else {
                    format!("{}+{}i", self.float_text(*a), self.float_text(*b))
                }
            }
            Value::Char(c) => c.to_string(),
            Value::Str(s) => s.to_string(),
            Value::String(s) => s.borrow().clone(),
            Value::Tuple(s) => {
                let items = s.borrow().clone();
                let parts: R<Vec<String>> = items.iter().map(|x| self.display(x)).collect();
                let parts = parts?;
                if parts.len() == 1 { format!("({},)", parts[0]) } else { format!("({})", parts.join(", ")) }
            }
            Value::Array(s) | Value::Vec(s) => {
                let items = s.borrow().clone();
                let parts: R<Vec<String>> = items.iter().map(|x| self.display(x)).collect();
                format!("[{}]", parts?.join(", "))
            }
            Value::Slice(s, a, b) => {
                let items: Vec<Value<'a>> = s.borrow()[*a..*b].to_vec();
                let parts: R<Vec<String>> = items.iter().map(|x| self.display(x)).collect();
                format!("[{}]", parts?.join(", "))
            }
            Value::Tensor(t) => self.tensor_text(&t.borrow()),
            Value::Struct(adt, slot) | Value::Variant(adt, _, slot) if self.prog.adt(*adt).module == 0 && matches!(self.prog.adt(*adt).name.as_str(), "IoError" | "TaskPanicked") => {
                if self.prog.adt(*adt).name == "IoError" {
                    let m = slot.borrow().first().cloned();
                    match m {
                        Some(m) => format!("io error: {}", self.text_of(&m)),
                        None => "io error".to_string(),
                    }
                } else {
                    "task panicked".to_string()
                }
            }
            Value::Struct(adt, _) | Value::Variant(adt, _, _) => {
                let adt = *adt;
                match self.find_trait_impl("Display", adt, "fmt") {
                    Some(fid) => {
                        let r = self.call_fn(fid, Vec::new(), vec![v.clone()])?;
                        self.display(&r)?
                    }
                    None => {
                        let name = self.prog.adt(adt).name.clone();
                        return panic(format!("`{name}` has no Display impl"));
                    }
                }
            }
            other => return panic(format!("cannot print a value of type {}", other.type_name())),
        })
    }

    /// The method `method` of the impl of the (module-0) trait `trait_name` for `adt`.
    pub fn find_trait_impl(&self, trait_name: &str, adt: AdtId, method: &str) -> Option<FnId> {
        let tr = self.prog.find_trait(trait_name)?;
        for imp in &self.prog.impls {
            if imp.trait_ref.as_ref().map(|t| t.trait_) != Some(tr) {
                continue;
            }
            if matches!(&imp.self_ty, Ty::Adt(a, _) if *a == adt) {
                return imp.methods.iter().find(|(n, _)| n == method).map(|(_, f)| *f);
            }
        }
        None
    }

    // ------------------------------------------------------------ equality and order

    pub fn values_eq(&mut self, a: &Value<'a>, b: &Value<'a>) -> R<'a, bool> {
        let a = self.deref(a.clone())?;
        let b = self.deref(b.clone())?;
        Ok(match (&a, &b) {
            (Value::Unit, Value::Unit) => true,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Int(x), Value::Int(y)) => x == y,
            (Value::Float(x), Value::Float(y)) => x == y,
            (Value::Complex(p, q), Value::Complex(r, s)) => p == r && q == s,
            (Value::Char(x), Value::Char(y)) => x == y,
            (Value::Str(_) | Value::String(_), Value::Str(_) | Value::String(_)) => {
                self.text_of(&a) == self.text_of(&b)
            }
            (Value::Tuple(x), Value::Tuple(y)) | (Value::Array(x), Value::Array(y)) | (Value::Vec(x), Value::Vec(y)) => {
                let (xs, ys) = (x.borrow().clone(), y.borrow().clone());
                self.all_eq(&xs, &ys)?
            }
            (Value::Struct(i, x), Value::Struct(j, y)) if i == j => match self.find_trait_impl("Eq", *i, "eq") {
                Some(f) => match self.call_fn(f, Vec::new(), vec![a.clone(), b.clone()])? {
                    Value::Bool(r) => r,
                    _ => false,
                },
                None => {
                    let (xs, ys) = (x.borrow().clone(), y.borrow().clone());
                    self.all_eq(&xs, &ys)?
                }
            },
            (Value::Variant(i, p, x), Value::Variant(j, q, y)) if i == j => {
                if p != q {
                    false
                } else {
                    let (xs, ys) = (x.borrow().clone(), y.borrow().clone());
                    self.all_eq(&xs, &ys)?
                }
            }
            _ => false,
        })
    }

    fn all_eq(&mut self, xs: &[Value<'a>], ys: &[Value<'a>]) -> R<'a, bool> {
        if xs.len() != ys.len() {
            return Ok(false);
        }
        for (x, y) in xs.iter().zip(ys) {
            if !self.values_eq(x, y)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn text_of(&self, v: &Value<'a>) -> String {
        match v {
            Value::Str(s) => s.to_string(),
            Value::String(s) => s.borrow().clone(),
            _ => String::new(),
        }
    }

    pub fn values_cmp(&mut self, a: &Value<'a>, b: &Value<'a>) -> R<'a, Option<std::cmp::Ordering>> {
        use std::cmp::Ordering;
        let a = self.deref(a.clone())?;
        let b = self.deref(b.clone())?;
        Ok(match (&a, &b) {
            (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
            (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
            (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
            (Value::Char(x), Value::Char(y)) => Some(x.cmp(y)),
            (Value::Str(_) | Value::String(_), Value::Str(_) | Value::String(_)) => {
                Some(self.text_of(&a).cmp(&self.text_of(&b)))
            }
            (Value::Tuple(x), Value::Tuple(y)) | (Value::Array(x), Value::Array(y)) | (Value::Vec(x), Value::Vec(y)) => {
                let (xs, ys) = (x.borrow().clone(), y.borrow().clone());
                for (p, q) in xs.iter().zip(&ys) {
                    match self.values_cmp(p, q)? {
                        Some(Ordering::Equal) => {}
                        other => return Ok(other),
                    }
                }
                Some(xs.len().cmp(&ys.len()))
            }
            (Value::Struct(i, _), Value::Struct(j, _)) if i == j => match self.find_trait_impl("Ord", *i, "lt") {
                Some(f) => {
                    let lt = matches!(self.call_fn(f, Vec::new(), vec![a.clone(), b.clone()])?, Value::Bool(true));
                    if lt {
                        Some(Ordering::Less)
                    } else if matches!(self.call_fn(f, Vec::new(), vec![b.clone(), a.clone()])?, Value::Bool(true)) {
                        Some(Ordering::Greater)
                    } else {
                        Some(Ordering::Equal)
                    }
                }
                None => None,
            },
            _ => None,
        })
    }
}

/// Match an impl's `self` type (with generic holes) against a concrete type,
/// recording what each generic parameter stands for.
pub fn match_ty(pattern: &Ty, actual: &Ty, binds: &mut Vec<(GenericId, Ty)>) -> bool {
    match (pattern, actual) {
        (Ty::Param(g), t) => {
            if let Some((_, existing)) = binds.iter().find(|(k, _)| k == g) {
                return existing == t;
            }
            binds.push((*g, t.clone()));
            true
        }
        (Ty::Adt(a, xs), Ty::Adt(b, ys)) => a == b && xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| match_ty(x, y, binds)),
        (Ty::Tuple(xs), Ty::Tuple(ys)) => xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| match_ty(x, y, binds)),
        (Ty::Array(x, _), Ty::Array(y, _)) | (Ty::Slice(x), Ty::Slice(y)) => match_ty(x, y, binds),
        (Ty::Tensor(x, _), Ty::Tensor(y, _)) => match_ty(x, y, binds),
        (Ty::Fn(ps, r), Ty::Fn(qs, s)) => ps.len() == qs.len() && ps.iter().zip(qs).all(|(x, y)| match_ty(x, y, binds)) && match_ty(r, s, binds),
        (a, b) => a == b,
    }
}
