//! Evaluating expressions, statements and patterns.

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::ast::BinOp;

/// Items per block in a parallel reduction (see `par_reduce`).
pub const PAR_BLOCK: usize = 4096;

pub(super) fn int_range(it: IntTy) -> (i128, i128) {
    match it {
        IntTy::I8 => (i8::MIN as i128, i8::MAX as i128),
        IntTy::I16 => (i16::MIN as i128, i16::MAX as i128),
        IntTy::I32 => (i32::MIN as i128, i32::MAX as i128),
        IntTy::I64 | IntTy::Isize => (i64::MIN as i128, i64::MAX as i128),
        IntTy::I128 => (i128::MIN, i128::MAX),
        IntTy::U8 => (0, u8::MAX as i128),
        IntTy::U16 => (0, u16::MAX as i128),
        IntTy::U32 => (0, u32::MAX as i128),
        IntTy::U64 | IntTy::Usize => (0, u64::MAX as i128),
        IntTy::U128 => (0, i128::MAX),
    }
}

fn bits(it: IntTy) -> u32 {
    match it {
        IntTy::I8 | IntTy::U8 => 8,
        IntTy::I16 | IntTy::U16 => 16,
        IntTy::I32 | IntTy::U32 => 32,
        IntTy::I64 | IntTy::U64 | IntTy::Isize | IntTy::Usize => 64,
        IntTy::I128 | IntTy::U128 => 128,
    }
}

/// Reduce `v` into the range of `it` the way `as` does (wrapping).
pub fn wrap_int(it: IntTy, v: i128) -> i128 {
    let b = bits(it);
    if b >= 128 {
        return v;
    }
    let (lo, _) = int_range(it);
    let modulus = 1i128 << b;
    let mut r = v.rem_euclid(modulus);
    if lo < 0 && r >= modulus / 2 {
        r -= modulus;
    }
    r
}

fn int_ty_of(t: &Ty) -> IntTy {
    match t {
        Ty::Int(i) => *i,
        _ => IntTy::I64,
    }
}

fn round_f32(single: bool, x: f64) -> f64 {
    if single {
        x as f32 as f64
    } else {
        x
    }
}

impl<'a> Interp<'a> {
    // ------------------------------------------------------------ blocks

    pub fn eval_block(&mut self, b: &'a Block, f: &Frame<'a>) -> R<'a, Value<'a>> {
        let needs_scope = b.stmts.iter().any(|s| matches!(s, Stmt::Let { .. } | Stmt::Item(_)));
        let child;
        let fr: &Frame<'a> = if needs_scope {
            child = f.child();
            &child
        } else {
            f
        };
        for s in &b.stmts {
            if let Stmt::Item(item) = s {
                if let ItemKind::Fn(func) = &item.kind {
                    self.register_fn(func);
                }
            }
        }
        let n = b.stmts.len();
        let mut last = Value::Unit;
        for (i, s) in b.stmts.iter().enumerate() {
            match s {
                Stmt::Let { pat, init, .. } => {
                    let v = match init {
                        Some(e) => self.eval_val(e, fr)?,
                        None => Value::Unit,
                    };
                    if !self.bind(pat, &v, fr, &fr.env)? {
                        return panic("a `let` pattern did not match the value");
                    }
                }
                Stmt::Expr(e) => {
                    if i + 1 == n {
                        last = self.eval_val(e, fr)?;
                    } else {
                        self.eval(e, fr)?;
                    }
                }
                Stmt::Item(_) => {}
            }
        }
        Ok(last)
    }

    /// Evaluate for use as a value: a place's tuple/array/struct shell is copied.
    pub fn eval_val(&mut self, e: &'a Expr, f: &Frame<'a>) -> R<'a, Value<'a>> {
        let v = self.eval(e, f)?;
        Ok(if matches!(e.kind, ExprKind::Ident(_) | ExprKind::Field { .. } | ExprKind::Index { .. }) {
            v.copy_shell()
        } else {
            v
        })
    }

    // ------------------------------------------------------------ expressions

    /// Evaluate `e`. If the checker says it is a box used as the value it holds (`Results::derefs`),
    /// the box is looked through.
    pub fn eval(&mut self, e: &'a Expr, f: &Frame<'a>) -> R<'a, Value<'a>> {
        let v = self.eval_plain(e, f)?;
        if self.res.derefs.is_empty() {
            return Ok(v);
        }
        match self.res.derefs.get(&NodeKey::of(e)) {
            Some(&n) => self.unbox(v, n),
            None => Ok(v),
        }
    }

    /// Look through `n` layers of `Box` (and references to them).
    fn unbox(&mut self, mut v: Value<'a>, n: u32) -> R<'a, Value<'a>> {
        for _ in 0..n {
            v = self.deref(v)?;
            match v {
                Value::Box(s) => {
                    let inner = s.borrow()[0].clone();
                    v = inner;
                }
                other => return Ok(other),
            }
        }
        Ok(v)
    }

    fn eval_plain(&mut self, e: &'a Expr, f: &Frame<'a>) -> R<'a, Value<'a>> {
        self.steps += 1;
        if self.steps > self.max_steps {
            return panic("step limit exceeded");
        }
        match &e.kind {
            ExprKind::Int { value, .. } => Ok(match self.ty_of(e, f) {
                Ty::Float(ft) => Value::Float(round_f32(ft == FloatTy::F32, *value as f64)),
                Ty::Complex(_) => Value::Complex(*value as f64, 0.0),
                _ => Value::Int(*value as i128),
            }),
            ExprKind::Float { value, .. } => Ok(match self.ty_of(e, f) {
                Ty::Float(FloatTy::F32) => Value::Float(*value as f32 as f64),
                Ty::Complex(_) => Value::Complex(*value, 0.0),
                _ => Value::Float(*value),
            }),
            ExprKind::Imag(v) => Ok(Value::Complex(0.0, *v)),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Char(c) => Ok(Value::Char(*c)),
            ExprKind::Str(pieces) => {
                let mut s = String::new();
                for p in pieces {
                    match p {
                        StrPiece::Lit(l) => s.push_str(l),
                        StrPiece::Interp(inner, fmt) => {
                            let v = self.eval(inner, f)?;
                            let ty = self.ty_of(inner, f);
                            match fmt {
                                None => s.push_str(&self.display_ty(&v, &ty)?),
                                Some(fmt) => s.push_str(&self.display_spec(&v, &ty, &fmt.spec)?),
                            }
                        }
                    }
                }
                Ok(Value::str(&s))
            }
            ExprKind::Ident(name) => self.ident(e, name, f),
            ExprKind::TypeApp { name, .. } => self.ident(e, &name.name, f),
            ExprKind::Tuple(es) => {
                let mut items = Vec::with_capacity(es.len());
                for x in es {
                    items.push(self.eval_val(x, f)?);
                }
                Ok(if items.is_empty() { Value::Unit } else { Value::tuple(items) })
            }
            ExprKind::Array(es) => {
                let mut items = Vec::with_capacity(es.len());
                for x in es {
                    items.push(self.eval_val(x, f)?);
                }
                Ok(Value::Array(Value::slot(items)))
            }
            ExprKind::ArrayRepeat { value, len } => {
                let v = self.eval_val(value, f)?;
                let n = match self.eval(len, f)? {
                    Value::Int(n) if n >= 0 => n as usize,
                    _ => return panic("an array length must be a non-negative integer"),
                };
                if n as u128 > crate::types::MAX_ARRAY_LEN {
                    return panic(format!("an array can have at most {} elements, not {n}", crate::types::MAX_ARRAY_LEN));
                }
                Ok(Value::Array(Value::slot((0..n).map(|_| v.copy_shell()).collect())))
            }
            ExprKind::Unary { op, expr } => self.unary(e, *op, expr, f),
            ExprKind::Binary { op, lhs, rhs } => self.binary(e, *op, lhs, rhs, f),
            ExprKind::Assign { op, lhs, rhs } => {
                self.assign(e, *op, lhs, rhs, f)?;
                Ok(Value::Unit)
            }
            ExprKind::Cast { expr, .. } => {
                let v = self.eval(expr, f)?;
                let to = self.ty_of(e, f);
                self.cast(v, &to)
            }
            ExprKind::Call { callee, args } => self.call(e, callee, args, f),
            ExprKind::MethodCall { recv, args, .. } => self.method_call(e, recv, args, f),
            ExprKind::Field { base, name } => self.field(e, base, &name.name, f),
            ExprKind::Index { base, indices } => self.index(e, base, indices, f),
            ExprKind::Try(inner) => {
                let v = self.eval_val(inner, f)?;
                match v {
                    Value::Variant(adt, idx, payload) if adt == self.result => {
                        if idx == 0 {
                            Ok(payload.borrow()[0].clone())
                        } else {
                            Err(Ctl::Return(Value::Variant(adt, idx, payload)))
                        }
                    }
                    Value::Variant(adt, idx, payload) if adt == self.option => {
                        if idx == 0 {
                            Ok(payload.borrow()[0].clone())
                        } else {
                            Err(Ctl::Return(Value::Variant(adt, idx, payload)))
                        }
                    }
                    other => panic(format!("`?` applied to a {}", other.type_name())),
                }
            }
            ExprKind::Range { start, end, inclusive } => {
                let a = match start {
                    Some(x) => self.int_of(x, f)?,
                    None => 0,
                };
                let b = match end {
                    Some(x) => self.int_of(x, f)? + if *inclusive { 1 } else { 0 },
                    None => i128::MAX / 2,
                };
                Ok(Value::Range(a, b))
            }
            ExprKind::Closure { params, body, .. } => Ok(Value::Closure(Rc::new(Closure {
                params,
                body,
                env: f.env.clone(),
                tyargs: f.tyargs.clone(),
            }))),
            ExprKind::Block(b) | ExprKind::Unsafe(b) => self.eval_block(b, f),
            ExprKind::If { cond, then, else_ } => {
                if self.cond(cond, f)? {
                    self.eval_block(then, f)
                } else if let Some(el) = else_ {
                    self.eval_val(el, f)
                } else {
                    Ok(Value::Unit)
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                let v = self.eval(scrutinee, f)?;
                for arm in arms {
                    let fr = f.child();
                    if self.bind(&arm.pat, &v, &fr, &fr.env)? {
                        if let Some(g) = &arm.guard {
                            if !self.cond(g, &fr)? {
                                continue;
                            }
                        }
                        return self.eval_val(&arm.body, &fr);
                    }
                }
                panic("no match arm matched the value")
            }
            ExprKind::Loop(b) => loop {
                match self.eval_block(b, f) {
                    Ok(_) | Err(Ctl::Continue) => {}
                    Err(Ctl::Break(v)) => return Ok(v.unwrap_or(Value::Unit)),
                    Err(other) => return Err(other),
                }
            },
            ExprKind::While { cond, body } => {
                while self.cond(cond, f)? {
                    match self.eval_block(body, f) {
                        Ok(_) | Err(Ctl::Continue) => {}
                        Err(Ctl::Break(_)) => break,
                        Err(other) => return Err(other),
                    }
                }
                Ok(Value::Unit)
            }
            ExprKind::For { pat, iter, guard, body, .. } => {
                self.for_loop(pat, iter, guard.as_deref(), body, f)?;
                Ok(Value::Unit)
            }
            ExprKind::Return(v) => {
                let r = match v {
                    Some(x) => self.eval_val(x, f)?,
                    None => Value::Unit,
                };
                Err(Ctl::Return(r))
            }
            ExprKind::Break(v) => {
                let r = match v {
                    Some(x) => Some(self.eval_val(x, f)?),
                    None => None,
                };
                Err(Ctl::Break(r))
            }
            ExprKind::Continue => Err(Ctl::Continue),
            ExprKind::MutBorrow(inner) => self.eval(inner, f),
            ExprKind::Own(inner) => self.eval(inner, f),
            ExprKind::ParReduce { op, pat, iter, body } => self.par_reduce(e, &op.name, pat, iter, body, f),
            ExprKind::Spawn(b) => {
                // tasks run to completion when spawned; what they print is kept until they are joined
                let fr = f.child();
                let sink = Rc::new(RefCell::new(Vec::new()));
                let real = std::mem::replace(&mut self.out, Box::new(TaskSink(sink.clone())));
                let result = self.eval_block(b, &fr);
                self.out = real;
                let outcome = match result {
                    Ok(v) | Err(Ctl::Return(v)) => Ok(v),
                    Err(Ctl::Panic(m)) => Err(m),
                    Err(_) => Ok(Value::Unit),
                };
                let cell: TaskCell<'a> = Rc::new(RefCell::new(Some((outcome, sink))));
                self.tasks.push(cell.clone());
                Ok(Value::Task(cell))
            }
            ExprKind::On { target, body } => {
                // there is no cluster yet: run here
                if !matches!(&target.kind, ExprKind::Ident(n) if n == "local") {
                    self.eval(target, f)?;
                }
                let fr = f.child();
                match self.eval_block(body, &fr) {
                    Err(Ctl::Return(v)) => Ok(v),
                    other => other,
                }
            }
        }
    }

    fn cond(&mut self, e: &'a Expr, f: &Frame<'a>) -> R<'a, bool> {
        match self.eval(e, f)? {
            Value::Bool(b) => Ok(b),
            other => panic(format!("expected a bool, found a {}", other.type_name())),
        }
    }

    fn int_of(&mut self, e: &'a Expr, f: &Frame<'a>) -> R<'a, i128> {
        match self.eval(e, f)? {
            Value::Int(i) => Ok(i),
            other => panic(format!("expected an integer, found a {}", other.type_name())),
        }
    }

    // ------------------------------------------------------------ names

    fn ident(&mut self, e: &'a Expr, name: &str, f: &Frame<'a>) -> R<'a, Value<'a>> {
        match self.res.expr_res.get(&NodeKey::of(e)) {
            Some(Res::Fn(fid, subst)) => Ok(Value::Func(*fid, Rc::new(self.substitute(subst, f)))),
            Some(Res::Const(cid)) => self.const_value(*cid),
            Some(Res::StructCtor(adt, _)) => Ok(Value::Ctor(*adt, None)),
            Some(Res::Variant(adt, i, _)) => {
                let unit = matches!(&self.prog.adt(*adt).kind, AdtKind::Enum(vs) if vs[*i].fields.is_empty());
                Ok(if unit { Value::variant(*adt, *i, vec![]) } else { Value::Ctor(*adt, Some(*i)) })
            }
            Some(Res::Builtin(_)) => panic("a built-in constructor cannot be used as a value"),
            Some(Res::Local(_)) | None => match f.env.get(name) {
                Some(Value::Ref(t)) => self.deref(Value::Ref(t)),
                Some(v) => Ok(v),
                None if name == "local" => Ok(Value::Unit),
                None => panic(format!("unbound variable `{name}`")),
            },
        }
    }

    // ------------------------------------------------------------ places

    /// The place an expression denotes, for assignment and `mut` arguments.
    pub fn place(&mut self, e: &'a Expr, f: &Frame<'a>) -> R<'a, RefTarget<'a>> {
        let mut t = self.place_plain(e, f)?;
        // a box used as the value it holds: the place is its slot
        if !self.res.derefs.is_empty() {
            for _ in 0..self.res.derefs.get(&NodeKey::of(e)).copied().unwrap_or(0) {
                let v = self.load(&t)?;
                match self.deref(v)? {
                    Value::Box(s) => t = RefTarget::Slot(s, 0),
                    _ => break,
                }
            }
        }
        Ok(t)
    }

    fn place_plain(&mut self, e: &'a Expr, f: &Frame<'a>) -> R<'a, RefTarget<'a>> {
        match &e.kind {
            ExprKind::Ident(name) => match f.env.find(name) {
                Some((env, i)) => {
                    // a variable that holds a reference writes through to its target
                    let cur = env.vars.borrow()[i].1.clone();
                    match cur {
                        Value::Ref(t) => {
                            let mut t = t;
                            loop {
                                let next = self.load(&t)?;
                                match next {
                                    Value::Ref(inner) => t = inner,
                                    _ => return Ok(t),
                                }
                            }
                        }
                        _ => Ok(RefTarget::Var(env, i)),
                    }
                }
                None => panic(format!("cannot take the place of `{name}`")),
            },
            ExprKind::Field { base, name } => {
                let b = self.eval(base, f)?;
                match b {
                    Value::Struct(adt, slot) => {
                        let idx = self.field_index(adt, &name.name)?;
                        Ok(RefTarget::Slot(slot, idx))
                    }
                    Value::Tuple(slot) => match name.name.parse::<usize>() {
                        Ok(i) if i < slot.borrow().len() => Ok(RefTarget::Slot(slot, i)),
                        _ => panic(format!("no field `{}`", name.name)),
                    },
                    other => panic(format!("cannot assign to a field of a {}", other.type_name())),
                }
            }
            ExprKind::Index { base, indices } => {
                let b = self.eval(base, f)?;
                match b {
                    Value::Vec(slot) | Value::Array(slot) | Value::Ptr(slot) => {
                        let i = self.checked_index(&slot, 0, slot.borrow().len(), indices, f)?;
                        Ok(RefTarget::Slot(slot, i))
                    }
                    Value::Slice(slot, s, en) => {
                        let i = self.checked_index(&slot, s, en, indices, f)?;
                        Ok(RefTarget::Slot(slot, i))
                    }
                    Value::Map(m) => {
                        let k = self.eval_val(&indices[0], f)?;
                        let key = self.map_key(&k)?;
                        Ok(RefTarget::MapEntry(m, key, Box::new(k)))
                    }
                    Value::Tensor(t) => {
                        let mut idx = Vec::new();
                        for i in indices {
                            idx.push(self.usize_of(i, f)?);
                        }
                        let flat = t.borrow().flat(&idx);
                        match flat {
                            Some(fl) => Ok(RefTarget::TensorElem(t, fl)),
                            None => panic("tensor index out of bounds"),
                        }
                    }
                    other => panic(format!("cannot index a {}", other.type_name())),
                }
            }
            _ => {
                // a temporary: give it a home so it can be borrowed
                let v = self.eval_val(e, f)?;
                let env = Env::root();
                env.declare("<temporary>", v);
                Ok(RefTarget::Var(env, 0))
            }
        }
    }

    fn usize_of(&mut self, e: &'a Expr, f: &Frame<'a>) -> R<'a, usize> {
        match self.eval(e, f)? {
            Value::Int(i) if i >= 0 => Ok(i as usize),
            Value::Int(i) => panic(format!("index {i} is negative")),
            other => panic(format!("an index must be an integer, found a {}", other.type_name())),
        }
    }

    fn checked_index(&mut self, _slot: &Slot<'a>, start: usize, end: usize, indices: &'a [Expr], f: &Frame<'a>) -> R<'a, usize> {
        let i = self.usize_of(&indices[0], f)?;
        let len = end - start;
        if i >= len {
            return panic(format!("index out of bounds: the length is {len} but the index is {i}"));
        }
        Ok(start + i)
    }

    pub fn field_index(&self, adt: AdtId, name: &str) -> R<'a, usize> {
        if let AdtKind::Struct(fields) = &self.prog.adt(adt).kind {
            if let Some(i) = fields.iter().position(|x| x.name == name) {
                return Ok(i);
            }
        }
        panic(format!("no field `{name}` on `{}`", self.prog.adt(adt).name))
    }

    pub fn map_key(&self, v: &Value<'a>) -> R<'a, MapKey> {
        Ok(match v {
            Value::Int(i) => MapKey::Int(*i),
            Value::Str(s) => MapKey::Str(s.to_string()),
            Value::String(s) => MapKey::Str(s.borrow().clone()),
            Value::Char(c) => MapKey::Char(*c),
            Value::Bool(b) => MapKey::Bool(*b),
            Value::Tuple(s) => {
                let items: R<Vec<MapKey>> = s.borrow().iter().map(|x| self.map_key(x)).collect();
                MapKey::Tuple(items?)
            }
            Value::Ref(t) => {
                let inner = self.load(t)?;
                return self.map_key(&inner);
            }
            other => return panic(format!("a {} cannot be a map key", other.type_name())),
        })
    }

    // ------------------------------------------------------------ calls

    fn arg_value(&mut self, a: &'a Expr, mode: ParamMode, f: &Frame<'a>) -> R<'a, Value<'a>> {
        let inner: &'a Expr = match &a.kind {
            ExprKind::MutBorrow(i) => i,
            _ => a,
        };
        // `mut v[2..]` is a mutable view of part of `v`, not a place
        let is_slicing = matches!(&inner.kind, ExprKind::Index { indices, .. } if indices.len() == 1 && matches!(indices[0].kind, ExprKind::Range { .. }));
        match mode {
            ParamMode::Mut if is_slicing => self.eval(inner, f),
            ParamMode::Mut => Ok(Value::Ref(self.place(inner, f)?)),
            ParamMode::Borrow => self.eval(inner, f),
            ParamMode::Own => self.eval_val(inner, f),
        }
    }

    fn eval_args(&mut self, fid: FnId, recv: Option<&'a Expr>, args: &'a [Expr], f: &Frame<'a>) -> R<'a, Vec<Value<'a>>> {
        let prog = self.prog;
        let sig = &prog.fn_(fid).sig;
        let mut out = Vec::with_capacity(args.len() + 1);
        if let (Some(r), Some(mode)) = (recv, sig.self_mode) {
            out.push(self.arg_value(r, mode, f)?);
        }
        for (i, a) in args.iter().enumerate() {
            let mode = sig.params.get(i).map(|p| p.mode).unwrap_or(ParamMode::Borrow);
            out.push(self.arg_value(a, mode, f)?);
        }
        Ok(out)
    }

    fn construct(&mut self, adt: AdtId, variant: Option<usize>, args: &'a [Expr], f: &Frame<'a>) -> R<'a, Value<'a>> {
        let mut items = Vec::with_capacity(args.len());
        for a in args {
            items.push(self.eval_val(a, f)?);
        }
        Ok(match variant {
            Some(i) => Value::variant(adt, i, items),
            None => Value::Struct(adt, Value::slot(items)),
        })
    }

    fn call(&mut self, _e: &'a Expr, callee: &'a Expr, args: &'a [Expr], f: &Frame<'a>) -> R<'a, Value<'a>> {
        match self.res.expr_res.get(&NodeKey::of(callee)) {
            Some(Res::Fn(fid, subst)) => {
                let sub = self.substitute(subst, f);
                let argv = self.eval_args(*fid, None, args, f)?;
                self.call_fn(*fid, sub, argv)
            }
            Some(Res::StructCtor(adt, _)) => self.construct(*adt, None, args, f),
            Some(Res::Variant(adt, i, _)) => self.construct(*adt, Some(*i), args, f),
            _ => {
                let fv = self.eval(callee, f)?;
                let mut argv = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.eval(a, f)?);
                }
                self.call_value(fv, argv)
            }
        }
    }

    fn method_call(&mut self, e: &'a Expr, recv: &'a Expr, args: &'a [Expr], f: &Frame<'a>) -> R<'a, Value<'a>> {
        match self.res.expr_res.get(&NodeKey::of(e)) {
            Some(Res::Fn(fid, subst)) => {
                let sub = self.substitute(subst, f);
                let has_self = self.prog.fn_(*fid).sig.self_mode.is_some();
                let argv = self.eval_args(*fid, if has_self { Some(recv) } else { None }, args, f)?;
                self.call_fn(*fid, sub, argv)
            }
            Some(Res::Variant(adt, i, _)) => self.construct(*adt, Some(*i), args, f),
            Some(Res::StructCtor(adt, _)) => self.construct(*adt, None, args, f),
            Some(Res::Builtin(kind)) => self.tensor_ctor(kind, e, args, f),
            _ => panic("an unresolved method call reached the interpreter"),
        }
    }

    // ------------------------------------------------------------ fields and indexing

    fn field(&mut self, e: &'a Expr, base: &'a Expr, name: &str, f: &Frame<'a>) -> R<'a, Value<'a>> {
        match self.res.expr_res.get(&NodeKey::of(e)) {
            Some(Res::Const(cid)) => return self.const_value(*cid),
            Some(Res::Fn(fid, subst)) => return Ok(Value::Func(*fid, Rc::new(self.substitute(subst, f)))),
            Some(Res::Variant(adt, i, _)) => {
                let unit = matches!(&self.prog.adt(*adt).kind, AdtKind::Enum(vs) if vs[*i].fields.is_empty());
                return Ok(if unit { Value::variant(*adt, *i, vec![]) } else { Value::Ctor(*adt, Some(*i)) });
            }
            _ => {}
        }
        let b = self.eval(base, f)?;
        match &b {
            Value::Struct(adt, slot) => {
                let i = self.field_index(*adt, name)?;
                let v = slot.borrow()[i].clone();
                Ok(v)
            }
            Value::Tuple(slot) => match name.parse::<usize>() {
                Ok(i) if i < slot.borrow().len() => Ok(slot.borrow()[i].clone()),
                _ => panic(format!("no field `{name}` on a tuple")),
            },
            Value::Vec(s) | Value::Array(s) if name == "len" => Ok(Value::Int(s.borrow().len() as i128)),
            Value::Slice(_, a, b) if name == "len" => Ok(Value::Int((b - a) as i128)),
            Value::Str(s) if name == "len" => Ok(Value::Int(s.len() as i128)),
            Value::String(s) if name == "len" => Ok(Value::Int(s.borrow().len() as i128)),
            Value::Tensor(t) if name == "len" => Ok(Value::Int(t.borrow().len() as i128)),
            other => panic(format!("no field `{name}` on a {}", other.type_name())),
        }
    }

    fn index(&mut self, e: &'a Expr, base: &'a Expr, indices: &'a [Expr], f: &Frame<'a>) -> R<'a, Value<'a>> {
        // `vec[1, 2, 3]` builds a vector
        if matches!(&base.kind, ExprKind::Ident(n) if n == "vec")
            && f.env.find("vec").is_none()
            && self.res.expr_res.get(&NodeKey::of(base)).is_none()
        {
            let mut items = Vec::with_capacity(indices.len());
            for i in indices {
                items.push(self.eval_val(i, f)?);
            }
            return Ok(Value::vec(items));
        }
        // overloaded indexing (maps, raw pointers, user types)
        if let Some(Res::Fn(fid, subst)) = self.res.expr_res.get(&NodeKey::of(e)) {
            let sub = self.substitute(subst, f);
            let b = self.eval(base, f)?;
            let i = self.eval(&indices[0], f)?;
            return self.call_fn(*fid, sub, vec![b, i]);
        }
        let b = self.eval(base, f)?;
        match b {
            Value::Vec(slot) | Value::Array(slot) | Value::Ptr(slot) => {
                let len = slot.borrow().len();
                self.index_into(&slot, 0, len, indices, f)
            }
            Value::Slice(slot, s, en) => self.index_into(&slot, s, en, indices, f),
            Value::Tensor(t) => {
                let mut idx = Vec::new();
                for i in indices {
                    idx.push(self.usize_of(i, f)?);
                }
                let flat = t.borrow().flat(&idx);
                match flat {
                    Some(fl) => self.load(&RefTarget::TensorElem(t, fl)),
                    None => panic(format!("tensor index {idx:?} is out of bounds")),
                }
            }
            other => panic(format!("cannot index a {}", other.type_name())),
        }
    }

    fn index_into(&mut self, slot: &Slot<'a>, start: usize, end: usize, indices: &'a [Expr], f: &Frame<'a>) -> R<'a, Value<'a>> {
        let len = end - start;
        let iv = self.eval(&indices[0], f)?;
        match iv {
            Value::Int(i) if i >= 0 && (i as usize) < len => Ok(slot.borrow()[start + i as usize].clone()),
            Value::Int(i) => panic(format!("index out of bounds: the length is {len} but the index is {i}")),
            Value::Range(a, b) => {
                let b = b.min(len as i128);
                if a < 0 || a > b || b > len as i128 {
                    return panic(format!("slice {a}..{b} is out of range for length {len}"));
                }
                Ok(Value::Slice(slot.clone(), start + a as usize, start + b as usize))
            }
            other => panic(format!("an index must be an integer, found a {}", other.type_name())),
        }
    }

    // ------------------------------------------------------------ operators

    fn unary(&mut self, e: &'a Expr, op: UnOp, inner: &'a Expr, f: &Frame<'a>) -> R<'a, Value<'a>> {
        if let Some(Res::Fn(fid, subst)) = self.res.expr_res.get(&NodeKey::of(e)) {
            let sub = self.substitute(subst, f);
            let v = self.eval(inner, f)?;
            return self.call_fn(*fid, sub, vec![v]);
        }
        let v = self.eval(inner, f)?;
        let ty = self.ty_of(e, f);
        match (op, v) {
            (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
            (UnOp::Neg, Value::Int(i)) => {
                let it = int_ty_of(&ty);
                let r = -i;
                self.fit(it, r)
            }
            (UnOp::Neg, Value::Float(x)) => Ok(Value::Float(-x)),
            (UnOp::Neg, Value::Complex(a, b)) => Ok(Value::Complex(-a, -b)),
            (UnOp::Neg, Value::Tensor(t)) => self.tensor_binop(BinOp::Mul, Value::Tensor(t), Value::Float(-1.0)),
            (UnOp::BitNot, Value::Int(i)) => {
                let it = int_ty_of(&ty);
                Ok(Value::Int(wrap_int(it, !i)))
            }
            (op, v) => panic(format!("cannot apply {op:?} to a {}", v.type_name())),
        }
    }

    fn fit(&self, it: IntTy, v: i128) -> R<'a, Value<'a>> {
        let (lo, hi) = int_range(it);
        if v < lo || v > hi {
            return panic(format!("integer overflow: {v} does not fit in {}", it.name()));
        }
        Ok(Value::Int(v))
    }

    fn binary(&mut self, e: &'a Expr, op: BinOp, lhs: &'a Expr, rhs: &'a Expr, f: &Frame<'a>) -> R<'a, Value<'a>> {
        if op == BinOp::And {
            return Ok(Value::Bool(self.cond(lhs, f)? && self.cond(rhs, f)?));
        }
        if op == BinOp::Or {
            return Ok(Value::Bool(self.cond(lhs, f)? || self.cond(rhs, f)?));
        }
        if let Some(Res::Fn(fid, subst)) = self.res.expr_res.get(&NodeKey::of(e)) {
            let sub = self.substitute(subst, f);
            let prog = self.prog;
            let sig = &prog.fn_(*fid).sig;
            let lm = sig.self_mode.unwrap_or(ParamMode::Borrow);
            let rm = sig.params.first().map(|p| p.mode).unwrap_or(ParamMode::Borrow);
            let l = self.arg_value(lhs, lm, f)?;
            let r = self.arg_value(rhs, rm, f)?;
            let name = prog.fn_(*fid).name.as_str();
            let truthy = |v: Value<'a>| matches!(v, Value::Bool(true));
            return Ok(match (name, op) {
                ("eq", BinOp::Eq) => self.call_fn(*fid, sub, vec![l, r])?,
                ("eq", _) => Value::Bool(!truthy(self.call_fn(*fid, sub, vec![l, r])?)),
                ("lt", BinOp::Lt) => self.call_fn(*fid, sub, vec![l, r])?,
                ("lt", BinOp::Gt) => self.call_fn(*fid, sub, vec![r, l])?,
                ("lt", BinOp::Le) => Value::Bool(!truthy(self.call_fn(*fid, sub, vec![r, l])?)),
                ("lt", _) => Value::Bool(!truthy(self.call_fn(*fid, sub, vec![l, r])?)),
                _ => self.call_fn(*fid, sub, vec![l, r])?,
            });
        }
        let l = self.eval(lhs, f)?;
        let r = self.eval(rhs, f)?;
        let lty = self.ty_of(lhs, f);
        self.bin_values(op, l, r, &lty)
    }

    pub fn bin_values(&mut self, op: BinOp, l: Value<'a>, r: Value<'a>, ty: &Ty) -> R<'a, Value<'a>> {
        use std::cmp::Ordering::*;
        match op {
            BinOp::Eq => return Ok(Value::Bool(self.values_eq(&l, &r)?)),
            BinOp::Ne => return Ok(Value::Bool(!self.values_eq(&l, &r)?)),
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let c = self.values_cmp(&l, &r)?;
                return Ok(Value::Bool(match (op, c) {
                    (_, None) => false,
                    (BinOp::Lt, Some(o)) => o == Less,
                    (BinOp::Le, Some(o)) => o != Greater,
                    (BinOp::Gt, Some(o)) => o == Greater,
                    (_, Some(o)) => o != Less,
                }));
            }
            _ => {}
        }
        match (&l, &r) {
            (Value::Int(a), Value::Int(b)) => self.int_op(op, *a, *b, int_ty_of(ty)),
            (Value::Float(a), Value::Float(b)) => {
                let single = matches!(ty, Ty::Float(FloatTy::F32));
                let x = match op {
                    BinOp::Add => a + b,
                    BinOp::Sub => a - b,
                    BinOp::Mul => a * b,
                    BinOp::Div => a / b,
                    BinOp::Rem => a % b,
                    BinOp::Pow => a.powf(*b),
                    _ => return panic(format!("cannot apply `{}` to floats", op.symbol())),
                };
                Ok(Value::Float(round_f32(single, x)))
            }
            (Value::Complex(a, b), Value::Complex(c, d)) => Ok(match op {
                BinOp::Add => Value::Complex(a + c, b + d),
                BinOp::Sub => Value::Complex(a - c, b - d),
                BinOp::Mul => Value::Complex(a * c - b * d, a * d + b * c),
                BinOp::Div => {
                    let den = c * c + d * d;
                    Value::Complex((a * c + b * d) / den, (b * c - a * d) / den)
                }
                _ => return panic(format!("cannot apply `{}` to complex numbers", op.symbol())),
            }),
            (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(match op {
                BinOp::BitAnd => *a & *b,
                BinOp::BitOr => *a | *b,
                BinOp::BitXor => *a ^ *b,
                _ => return panic(format!("cannot apply `{}` to bools", op.symbol())),
            })),
            (Value::Tensor(_), _) | (_, Value::Tensor(_)) => self.tensor_binop(op, l, r),
            _ => panic(format!("cannot apply `{}` to a {} and a {}", op.symbol(), l.type_name(), r.type_name())),
        }
    }

    fn int_op(&self, op: BinOp, a: i128, b: i128, it: IntTy) -> R<'a, Value<'a>> {
        let overflow = || -> R<'a, Value<'a>> { panic(format!("integer overflow in `{}` on {}", op.symbol(), it.name())) };
        let r = match op {
            BinOp::Add => match a.checked_add(b) {
                Some(x) => x,
                None => return overflow(),
            },
            BinOp::Sub => match a.checked_sub(b) {
                Some(x) => x,
                None => return overflow(),
            },
            BinOp::Mul => match a.checked_mul(b) {
                Some(x) => x,
                None => return overflow(),
            },
            BinOp::Div | BinOp::Rem => {
                if b == 0 {
                    return panic("division by zero");
                }
                if op == BinOp::Div { a / b } else { a % b }
            }
            BinOp::Pow => {
                if b < 0 {
                    return panic("integer power with a negative exponent");
                }
                match a.checked_pow(b.min(u32::MAX as i128) as u32) {
                    Some(x) => x,
                    None => return overflow(),
                }
            }
            BinOp::BitAnd => a & b,
            BinOp::BitOr => a | b,
            BinOp::BitXor => a ^ b,
            BinOp::Shl => {
                if b < 0 || b >= bits(it) as i128 {
                    return panic(format!("shift amount {b} is too large for {}", it.name()));
                }
                return Ok(Value::Int(wrap_int(it, a << b)));
            }
            BinOp::Shr => {
                if b < 0 || b >= bits(it) as i128 {
                    return panic(format!("shift amount {b} is too large for {}", it.name()));
                }
                a >> b
            }
            _ => return panic(format!("cannot apply `{}` to integers", op.symbol())),
        };
        self.fit(it, r)
    }

    fn cast(&mut self, v: Value<'a>, to: &Ty) -> R<'a, Value<'a>> {
        let v = self.deref(v)?;
        Ok(match (v, to) {
            (Value::Int(i), Ty::Int(it)) => Value::Int(wrap_int(*it, i)),
            (Value::Int(i), Ty::Float(ft)) => Value::Float(round_f32(*ft == FloatTy::F32, i as f64)),
            (Value::Int(i), Ty::Complex(_)) => Value::Complex(i as f64, 0.0),
            (Value::Float(x), Ty::Int(it)) => {
                let (lo, hi) = int_range(*it);
                let r = if x.is_nan() { 0 } else if x >= hi as f64 { hi } else if x <= lo as f64 { lo } else { x as i128 };
                Value::Int(r)
            }
            (Value::Float(x), Ty::Float(ft)) => Value::Float(round_f32(*ft == FloatTy::F32, x)),
            (Value::Float(x), Ty::Complex(_)) => Value::Complex(x, 0.0),
            (Value::Bool(b), Ty::Int(_)) => Value::Int(b as i128),
            (Value::Char(c), Ty::Int(it)) => Value::Int(wrap_int(*it, c as i128)),
            (Value::Int(i), Ty::Char) => match char::from_u32(i as u32) {
                Some(c) => Value::Char(c),
                None => return panic(format!("{i} is not a valid character")),
            },
            (Value::Complex(a, b), Ty::Complex(_)) => Value::Complex(a, b),
            (v, to) => {
                let _ = to;
                v
            }
        })
    }

    // ------------------------------------------------------------ assignment

    fn assign(&mut self, e: &'a Expr, op: Option<BinOp>, lhs: &'a Expr, rhs: &'a Expr, f: &Frame<'a>) -> R<'a, ()> {
        let rv = self.eval_val(rhs, f)?;
        let target = self.place(lhs, f)?;
        let Some(bop) = op else {
            return self.store(&target, rv);
        };
        // `counts[k] += 1` starts a missing map entry at zero
        if let RefTarget::MapEntry(m, k, key_value) = &target {
            let missing = !m.borrow().index.contains_key(k);
            if missing {
                let ty = self.ty_of(lhs, f);
                let zero = self.default_value(&ty)?;
                let mut mm = m.borrow_mut();
                let at = mm.entries.len();
                mm.entries.push(((**key_value).clone(), zero));
                mm.index.insert(k.clone(), at);
            }
        }
        let cur = self.load(&target)?;
        let result = if let Some(Res::Fn(fid, subst)) = self.res.expr_res.get(&NodeKey::of(e)) {
            let sub = self.substitute(subst, f);
            self.call_fn(*fid, sub, vec![cur, rv])?
        } else {
            let ty = self.ty_of(lhs, f);
            self.bin_values(bop, cur, rv, &ty)?
        };
        self.store(&target, result)
    }

    pub fn default_value(&self, ty: &Ty) -> R<'a, Value<'a>> {
        Ok(match ty {
            Ty::Int(_) => Value::Int(0),
            Ty::Float(_) => Value::Float(0.0),
            Ty::Complex(_) => Value::Complex(0.0, 0.0),
            Ty::Bool => Value::Bool(false),
            Ty::Char => Value::Char('\0'),
            Ty::Str => Value::str(""),
            Ty::Tuple(ts) if ts.is_empty() => Value::Unit,
            Ty::Tuple(ts) => Value::tuple(ts.iter().map(|t| self.default_value(t)).collect::<R<Vec<_>>>()?),
            Ty::Adt(id, _) => match self.prog.adt(*id).name.as_str() {
                "String" => Value::string(""),
                "Vec" => Value::vec(vec![]),
                other => return panic(format!("`{other}` has no default value")),
            },
            other => return panic(format!("no default value for {other:?}")),
        })
    }

    // ------------------------------------------------------------ loops

    /// The items of a sequence, for loops and reductions.
    fn sequence(&mut self, v: Value<'a>) -> R<'a, Vec<Value<'a>>> {
        let v = self.deref(v)?;
        Ok(match v {
            Value::Vec(s) | Value::Array(s) => s.borrow().clone(),
            Value::Slice(s, a, b) => s.borrow()[a..b].to_vec(),
            other => return panic(format!("a {} is not iterable", other.type_name())),
        })
    }

    fn for_loop(&mut self, pat: &'a Pattern, iter: &'a Expr, guard: Option<&'a Expr>, body: &'a Block, f: &Frame<'a>) -> R<'a, ()> {
        let (inner, mut_iter): (&'a Expr, bool) = match &iter.kind {
            ExprKind::MutBorrow(i) => (i, true),
            _ => (iter, false),
        };
        let it = self.eval(inner, f)?;
        // elements of a `mut` loop (and of `par_chunks`) are references into the collection
        let by_ref = mut_iter;
        match it {
            Value::Range(a, b) => {
                let mut i = a;
                while i < b {
                    let fr = f.child();
                    if !self.bind(pat, &Value::Int(i), &fr, &fr.env)? {
                        return panic("loop pattern did not match");
                    }
                    i += 1;
                    if let Some(g) = guard {
                        if !self.cond(g, &fr)? {
                            continue;
                        }
                    }
                    match self.eval_block(body, &fr) {
                        Ok(_) | Err(Ctl::Continue) => {}
                        Err(Ctl::Break(_)) => break,
                        Err(other) => return Err(other),
                    }
                }
                Ok(())
            }
            other => {
                let (slot, start, end) = match self.deref(other)? {
                    Value::Vec(s) | Value::Array(s) => {
                        let n = s.borrow().len();
                        (s, 0, n)
                    }
                    Value::Slice(s, a, b) => (s, a, b),
                    v => return panic(format!("a {} is not iterable", v.type_name())),
                };
                for i in start..end {
                    let item = if by_ref { Value::Ref(RefTarget::Slot(slot.clone(), i)) } else { slot.borrow()[i].clone() };
                    let fr = f.child();
                    if !self.bind(pat, &item, &fr, &fr.env)? {
                        return panic("loop pattern did not match");
                    }
                    if let Some(g) = guard {
                        if !self.cond(g, &fr)? {
                            continue;
                        }
                    }
                    match self.eval_block(body, &fr) {
                        Ok(_) | Err(Ctl::Continue) => {}
                        Err(Ctl::Break(_)) => break,
                        Err(other) => return Err(other),
                    }
                }
                Ok(())
            }
        }
    }

    /// Combine two partial results of a reduction.
    fn reduce_pair(&mut self, op: &str, a: Value<'a>, x: Value<'a>, ty: &Ty) -> R<'a, Value<'a>> {
        Ok(match op {
            "sum" => self.bin_values(BinOp::Add, a, x, ty)?,
            "prod" => self.bin_values(BinOp::Mul, a, x, ty)?,
            "min" => {
                if self.values_cmp(&x, &a)? == Some(std::cmp::Ordering::Less) { x } else { a }
            }
            "max" => {
                if self.values_cmp(&x, &a)? == Some(std::cmp::Ordering::Greater) { x } else { a }
            }
            "any" => Value::Bool(matches!(a, Value::Bool(true)) || matches!(x, Value::Bool(true))),
            "all" => Value::Bool(matches!(a, Value::Bool(true)) && matches!(x, Value::Bool(true))),
            other => return panic(format!("unknown reduction `{other}`")),
        })
    }

    /// A parallel reduction. The iteration is cut into blocks of `PAR_BLOCK` items; each block
    /// is reduced in order starting from its first item, and the block results are combined in
    /// block order. This is the definition of the result, so it does not depend on how many
    /// threads a parallel implementation uses: floating-point sums come out the same on any
    /// machine.
    fn par_reduce(&mut self, e: &'a Expr, op: &str, pat: &'a Pattern, iter: &'a Expr, body: &'a Expr, f: &Frame<'a>) -> R<'a, Value<'a>> {
        let ty = self.ty_of(e, f);
        let it = self.eval(iter, f)?;
        let mut blocks: Vec<Value<'a>> = Vec::new();
        let mut cur: Option<Value<'a>> = None;
        let mut in_block = 0usize;
        let mut items: Box<dyn Iterator<Item = Value<'a>>> = match it {
            Value::Range(a, b) => Box::new((a..b).map(Value::Int)),
            other => Box::new(self.sequence(other)?.into_iter()),
        };
        while let Some(item) = items.next() {
            let fr = f.child();
            self.bind(pat, &item, &fr, &fr.env)?;
            let x = self.eval_val(body, &fr)?;
            cur = Some(match cur.take() {
                None => x,
                Some(a) => self.reduce_pair(op, a, x, &ty)?,
            });
            in_block += 1;
            if in_block == PAR_BLOCK {
                blocks.push(cur.take().unwrap());
                in_block = 0;
            }
        }
        if let Some(c) = cur {
            blocks.push(c);
        }
        let mut acc: Option<Value<'a>> = None;
        for b in blocks {
            acc = Some(match acc.take() {
                None => b,
                Some(a) => self.reduce_pair(op, a, b, &ty)?,
            });
        }
        match acc {
            Some(v) => Ok(v),
            None => match op {
                "sum" => self.default_value(&ty),
                "prod" => Ok(match ty {
                    Ty::Float(_) => Value::Float(1.0),
                    _ => Value::Int(1),
                }),
                "any" => Ok(Value::Bool(false)),
                "all" => Ok(Value::Bool(true)),
                other => panic(format!("`{other}` of an empty range")),
            },
        }
    }

    // ------------------------------------------------------------ patterns

    fn is_unit_variant_pattern(&self, p: &Pattern, name: &str, f: &Frame<'a>) -> Option<(AdtId, usize)> {
        match self.pat_ty(p, f) {
            Ty::Adt(id, _) => match &self.prog.adt(id).kind {
                AdtKind::Enum(vs) => vs.iter().position(|v| v.name == name && v.fields.is_empty()).map(|i| (id, i)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Match `v` against `p`, declaring the bindings in `env`.
    pub fn bind(&mut self, p: &'a Pattern, v: &Value<'a>, f: &Frame<'a>, env: &Rc<Env<'a>>) -> R<'a, bool> {
        match &p.kind {
            PatKind::Wild => Ok(true),
            PatKind::Ident { name, mutable } => {
                if !*mutable {
                    if let Some((adt, idx)) = self.is_unit_variant_pattern(p, name, f) {
                        let d = self.deref(v.clone())?;
                        return Ok(matches!(d, Value::Variant(a, i, _) if a == adt && i == idx));
                    }
                }
                env.declare(name, v.clone());
                Ok(true)
            }
            PatKind::Lit(e) => {
                let lit = self.eval(e, f)?;
                self.values_eq(&lit, v)
            }
            PatKind::Range { lo, hi, inclusive } => {
                let l = self.eval(lo, f)?;
                let h = self.eval(hi, f)?;
                let x = self.deref(v.clone())?;
                let a = self.values_cmp(&x, &l)?;
                let b = self.values_cmp(&x, &h)?;
                use std::cmp::Ordering::*;
                Ok(matches!(a, Some(Greater | Equal))
                    && if *inclusive { matches!(b, Some(Less | Equal)) } else { matches!(b, Some(Less)) })
            }
            PatKind::Tuple(ps) => {
                let d = self.deref(v.clone())?;
                match d {
                    Value::Unit if ps.is_empty() => Ok(true),
                    Value::Tuple(slot) => {
                        let items = slot.borrow().clone();
                        if items.len() != ps.len() {
                            return Ok(false);
                        }
                        for (sub, item) in ps.iter().zip(&items) {
                            if !self.bind(sub, item, f, env)? {
                                return Ok(false);
                            }
                        }
                        Ok(true)
                    }
                    _ => Ok(false),
                }
            }
            PatKind::Variant { path, args } => {
                let d = self.deref(v.clone())?;
                let name = &path.last().unwrap().name;
                let (matches, payload) = match &d {
                    Value::Variant(adt, idx, slot) => {
                        let vname = match &self.prog.adt(*adt).kind {
                            AdtKind::Enum(vs) => vs[*idx].name.as_str(),
                            _ => "",
                        };
                        (vname == name, slot.clone())
                    }
                    Value::Struct(adt, slot) => (self.prog.adt(*adt).name == *name, slot.clone()),
                    _ => return Ok(false),
                };
                if !matches {
                    return Ok(false);
                }
                let items = payload.borrow().clone();
                if items.len() != args.len() {
                    return Ok(false);
                }
                for (sub, item) in args.iter().zip(&items) {
                    if !self.bind(sub, item, f, env)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            PatKind::Path(path) => {
                let d = self.deref(v.clone())?;
                let name = &path.last().unwrap().name;
                Ok(match &d {
                    Value::Variant(adt, idx, _) => matches!(&self.prog.adt(*adt).kind, AdtKind::Enum(vs) if vs[*idx].name == *name),
                    _ => false,
                })
            }
            PatKind::Or(alts) => {
                for a in alts {
                    if self.bind(a, v, f, env)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }
}
