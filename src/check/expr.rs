//! Type inference for blocks, statements and expressions.

use super::lower::prim_type;
use super::*;
use crate::infer::VarKind;

/// What the left side of `a.b` refers to when it is not a value.
enum Head {
    Module(usize),
    Type(Ty),
    Enum(AdtId, Vec<Ty>),
}

fn op_trait(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "Add",
        BinOp::Sub => "Sub",
        BinOp::Mul => "Mul",
        BinOp::Div => "Div",
        BinOp::Rem => "Rem",
        BinOp::Pow => "Pow",
        BinOp::MatMul => "MatMul",
        BinOp::BitAnd => "BitAnd",
        BinOp::BitOr => "BitOr",
        BinOp::BitXor => "BitXor",
        BinOp::Shl => "Shl",
        BinOp::Shr => "Shr",
        _ => "",
    }
}

impl<'a> Checker<'a> {
    // ------------------------------------------------------------ helpers

    fn err_ty(&mut self, span: Span, msg: impl Into<String>) -> Ty {
        self.err(span, msg);
        Ty::Error
    }

    fn mismatch(&mut self, span: Span, expected: &Ty, actual: &Ty) {
        let (e, a) = (self.show(expected), self.show(actual));
        let ex = self.infer.resolve(expected);
        let ac = self.infer.resolve(actual);
        let hint = match (&ex, &ac) {
            (Ty::Float(_), Ty::Int(_)) | (Ty::Int(_), Ty::Float(_)) | (Ty::Int(_), Ty::Int(_)) => {
                Some(format!("convert explicitly, e.g. `x as {e}`"))
            }
            (Ty::Str, t) if self.is_string(t) => None,
            (t, Ty::Str) if self.is_string(t) => Some("make an owned string with `.to_string()`".to_string()),
            _ => None,
        };
        let msg = format!("mismatched types: expected `{e}`, found `{a}`");
        match hint {
            Some(h) => self.err_hint(span, msg, h),
            None => self.err(span, msg),
        }
    }

    /// Unify `actual` with `expected`, applying the implicit coercions: `Never` to
    /// anything, arrays and vectors to slices, `String` to `str`.
    pub(super) fn coerce(&mut self, actual: &Ty, expected: &Ty, span: Span) {
        let a = self.infer.shallow(actual);
        let x = self.infer.shallow(expected);
        match (&a, &x) {
            (Ty::Never, _) => return,
            (Ty::Array(ea, _), Ty::Slice(es)) => {
                if !self.infer.unify(ea, es) {
                    self.mismatch(span, &x, &a);
                }
                return;
            }
            (Ty::Adt(..), Ty::Slice(es)) => {
                if let Some(elem) = self.is_vec(&a) {
                    if !self.infer.unify(&elem, es) {
                        self.mismatch(span, &x, &a);
                    }
                    return;
                }
            }
            (Ty::Adt(..), Ty::Str) if self.is_string(&a) => return,
            _ => {}
        }
        if !self.infer.unify(&a, &x) {
            self.mismatch(span, &x, &a);
        }
    }

    pub(super) fn expr_coerce(&mut self, e: &'a Expr, expected: &Ty) -> Ty {
        let t = self.expr(e, Some(expected));
        self.coerce(&t, expected, e.span);
        t
    }

    /// Join the types of two branches (`if`/`else`, match arms).
    fn join(&mut self, a: Ty, b: Ty, span: Span) -> Ty {
        let sa = self.infer.shallow(&a);
        let sb = self.infer.shallow(&b);
        match (&sa, &sb) {
            (Ty::Never, _) => b,
            (_, Ty::Never) => a,
            _ => {
                self.coerce(&b, &a, span);
                a
            }
        }
    }

    fn adt_named(&self, name: &str) -> Option<AdtId> {
        self.prog.find_adt(name)
    }

    fn lookup_body_value(&self, name: &str) -> Option<ValueItem> {
        for scope in self.body.local_fns.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(*v);
            }
        }
        self.lookup_value(self.body.cx.module, name)
    }

    // ------------------------------------------------------------ blocks

    pub(super) fn block(&mut self, b: &'a Block, exp: Option<&Ty>) -> Ty {
        self.push_scope();
        self.body.local_fns.push(HashMap::new());
        for s in &b.stmts {
            if let Stmt::Item(item) = s {
                self.nested_item(item);
            }
        }
        let n = b.stmts.len();
        let mut diverges = false;
        let mut result = None;
        for (i, s) in b.stmts.iter().enumerate() {
            let last = i + 1 == n;
            match s {
                Stmt::Let { pat, ty, init, span } => {
                    let cx = self.body.cx.clone();
                    let declared = ty.as_ref().map(|t| self.lower_ty(t, &cx));
                    let t = match (init, declared) {
                        (Some(e), Some(d)) => {
                            self.expr_coerce(e, &d);
                            d
                        }
                        (Some(e), None) => self.expr(e, None),
                        (None, Some(d)) => d,
                        (None, None) => self.fresh(*span),
                    };
                    if matches!(self.infer.shallow(&t), Ty::Never) {
                        diverges = true;
                    }
                    self.pattern(pat, &t, false);
                }
                Stmt::Expr(e) => {
                    let t = self.expr(e, if last { exp } else { None });
                    if matches!(self.infer.shallow(&t), Ty::Never) {
                        diverges = true;
                    }
                    if last {
                        result = Some(t);
                    }
                }
                Stmt::Item(_) => {}
            }
        }
        self.body.local_fns.pop();
        self.pop_scope();
        match result {
            Some(t) => t,
            None if diverges => Ty::Never,
            None => Ty::unit(),
        }
    }

    fn nested_item(&mut self, item: &'a Item) {
        match &item.kind {
            ItemKind::Fn(f) => {
                let module = self.body.cx.module;
                let id = self.new_fn(f, module, false, Parent::Free);
                let base = LowerCx {
                    module,
                    ..LowerCx::default()
                };
                let (sig, cx) = self.lower_fn_sig(f, &base);
                self.prog.fns[id.idx()].sig = sig;
                self.fn_cx[id.idx()] = cx;
                if let Some(scope) = self.body.local_fns.last_mut() {
                    scope.insert(f.name.name.clone(), ValueItem::Fn(id));
                }
                self.queued.push(id);
            }
            _ => self.err_hint(
                item.span,
                "this kind of item cannot be declared inside a function yet",
                "move it to the top level of the file",
            ),
        }
    }

    // ------------------------------------------------------------ expressions

    pub(super) fn expr(&mut self, e: &'a Expr, exp: Option<&Ty>) -> Ty {
        let t = self.expr_inner(e, exp);
        if matches!(e.kind, ExprKind::Call { .. } | ExprKind::MethodCall { .. }) {
            self.solve_bound_args();
        }
        self.record(e, &t);
        if let ExprKind::Int { value, .. } = &e.kind {
            self.body.int_lits.push((NodeKey::of(e), *value, e.span, false));
        }
        t
    }

    fn numeric_hint(&self, exp: Option<&Ty>) -> Option<Ty> {
        let t = self.infer.shallow(exp?);
        match &t {
            Ty::Int(_) | Ty::Float(_) | Ty::Complex(_) => Some(t),
            Ty::Var(v) if self.infer.kind_of(*v) != VarKind::General => Some(t),
            _ => None,
        }
    }

    fn expr_inner(&mut self, e: &'a Expr, exp: Option<&Ty>) -> Ty {
        match &e.kind {
            ExprKind::Int { value: _, suffix } => match suffix.as_deref() {
                Some(s) => IntTy::from_name(s).map(Ty::Int).unwrap_or(Ty::Error),
                None => {
                    let v = self.infer.fresh(VarKind::Int, e.span);
                    if let Some(h) = self.numeric_hint(exp) {
                        self.infer.unify(&v, &h);
                    }
                    v
                }
            },
            ExprKind::Float { suffix, .. } => match suffix.as_deref() {
                Some("f32") => Ty::Float(FloatTy::F32),
                Some(_) => Ty::Float(FloatTy::F64),
                None => {
                    let v = self.infer.fresh(VarKind::Float, e.span);
                    if let Some(h) = self.numeric_hint(exp) {
                        self.infer.unify(&v, &h);
                    }
                    v
                }
            },
            ExprKind::Imag(_) => Ty::Complex(FloatTy::F64),
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Char(_) => Ty::Char,
            ExprKind::Str(pieces) => {
                let display = self.prog.find_trait("Display");
                for p in pieces {
                    if let StrPiece::Interp(inner, fmt) = p {
                        let t = self.expr(inner, None);
                        if let Some(f) = fmt {
                            self.body.fmt_checks.push((t.clone(), f.clone()));
                        }
                        if let Some(tr) = display {
                            self.body.obligations.push(Obligation {
                                ty: t,
                                trait_: tr,
                                args: vec![],
                                span: inner.span,
                            });
                        }
                    }
                }
                Ty::Str
            }
            ExprKind::Ident(name) => self.ident_value(e, name, None),
            ExprKind::TypeApp { name, args } => {
                let cx = self.body.cx.clone();
                let tys: Vec<Ty> = args.iter().map(|a| self.lower_ty(a, &cx)).collect();
                self.ident_value(e, &name.name, Some(&tys))
            }
            ExprKind::Tuple(es) => {
                let hints: Vec<Option<Ty>> = match exp.map(|t| self.infer.shallow(t)) {
                    Some(Ty::Tuple(ts)) if ts.len() == es.len() => ts.into_iter().map(Some).collect(),
                    _ => es.iter().map(|_| None).collect(),
                };
                let tys = es
                    .iter()
                    .zip(hints)
                    .map(|(x, h)| self.expr(x, h.as_ref()))
                    .collect();
                Ty::Tuple(tys)
            }
            ExprKind::Array(es) => {
                let elem_hint = match exp.map(|t| self.infer.shallow(t)) {
                    Some(Ty::Array(t, _)) | Some(Ty::Slice(t)) => Some(*t),
                    _ => None,
                };
                let elem = elem_hint.unwrap_or_else(|| self.fresh(e.span));
                for x in es {
                    self.expr_coerce(x, &elem);
                }
                Ty::Array(Box::new(elem), Some(es.len() as u128))
            }
            ExprKind::ArrayRepeat { value, len } => {
                let hint = match exp.map(|t| self.infer.shallow(t)) {
                    Some(Ty::Array(t, _)) | Some(Ty::Slice(t)) => Some(*t),
                    _ => None,
                };
                let vt = self.expr(value, hint.as_ref());
                self.expr_coerce(len, &Ty::usize());
                let n = match &len.kind {
                    ExprKind::Int { value, .. } => Some(*value),
                    _ => None,
                };
                if let Some(n) = n {
                    if n > MAX_ARRAY_LEN {
                        self.err_hint(len.span, format!("an array can have at most {MAX_ARRAY_LEN} elements, not {n}"), "use a `Vec` for more");
                    }
                }
                Ty::Array(Box::new(vt), n)
            }
            ExprKind::Unary { op, expr: inner } => self.unary(e, *op, inner, exp),
            ExprKind::Binary { op, lhs, rhs } => self.binary(e, *op, lhs, rhs, exp),
            ExprKind::Assign { op, lhs, rhs } if is_tensor_slice_target(lhs) => {
                // `t[0..2, 1] = other` (a tensor of that shape) or `= 0.0` (every element)
                let lt = self.expr(lhs, None);
                self.check_mutable_place(lhs, "assign to");
                if op.is_some() {
                    self.err_hint(e.span, "an operator cannot be combined with assignment to part of a tensor", "write it out: `t[0..2, 1] = t[0..2, 1] + x`");
                    self.expr(rhs, None);
                } else if let Ty::Tensor(elem, _) = self.infer.shallow(&lt) {
                    let rt = self.expr(rhs, Some(&elem));
                    if matches!(self.infer.shallow(&rt), Ty::Tensor(..)) {
                        self.coerce(&rt, &lt, rhs.span);
                    } else {
                        self.coerce(&rt, &elem, rhs.span);
                    }
                } else {
                    // not a tensor (a slice of a vector, say): as for any other assignment
                    self.expr_coerce(rhs, &lt);
                }
                Ty::unit()
            }
            ExprKind::Assign { op, lhs, rhs } => {
                let lt = self.expr(lhs, None);
                self.check_mutable_place(lhs, "assign to");
                match op {
                    None => {
                        self.expr_coerce(rhs, &lt);
                    }
                    Some(bop) => {
                        let rt = self.expr(rhs, Some(&lt));
                        let res = self.binary_result(e, *bop, &lt, &rt, lhs.span, rhs.span);
                        self.coerce(&res, &lt, e.span);
                    }
                }
                Ty::unit()
            }
            ExprKind::Cast { expr: inner, ty } => {
                let from = self.expr(inner, None);
                let cx = self.body.cx.clone();
                let to = self.lower_ty(ty, &cx);
                self.check_cast(&from, &to, e.span);
                to
            }
            ExprKind::Call { callee, args } => self.call(e, callee, args),
            ExprKind::MethodCall { recv, name, args } => self.method_call(e, recv, name, args),
            ExprKind::Field { base, name } => self.field(e, base, name),
            ExprKind::Index { base, indices } => self.index(e, base, indices),
            ExprKind::Try(inner) => self.try_expr(e, inner),
            ExprKind::Range { start, end, .. } => {
                let elem = self.fresh(e.span);
                for x in [start, end].into_iter().flatten() {
                    self.expr_coerce(x, &elem);
                }
                match self.adt_named("Range") {
                    Some(r) => Ty::Adt(r, vec![elem]),
                    None => Ty::Error,
                }
            }
            ExprKind::Closure { params, ret, body } => self.closure(e, params, ret.as_ref(), body, exp),
            ExprKind::Block(b) => self.block(b, exp),
            ExprKind::Unsafe(b) => {
                self.body.unsafe_depth += 1;
                let t = self.block(b, exp);
                self.body.unsafe_depth -= 1;
                t
            }
            ExprKind::If { cond, then, else_ } => {
                self.expr_coerce(cond, &Ty::Bool);
                match else_ {
                    None => {
                        self.block(then, None);
                        Ty::unit()
                    }
                    Some(el) => {
                        let a = self.block(then, exp);
                        let b = self.expr(el, exp);
                        self.join(a, b, el.span)
                    }
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                let mut st = self.expr(scrutinee, None);
                // matching on a box looks through it, unless every arm just names the whole box
                if arms.iter().any(|a| !matches!(a.pat.kind, PatKind::Wild | PatKind::Ident { .. })) {
                    let mut cur = self.infer.shallow(&st);
                    let mut layers = 0u32;
                    while let Some(inner) = self.box_inner(&cur) {
                        cur = self.infer.shallow(&inner);
                        layers += 1;
                    }
                    if layers > 0 {
                        let scrutinee: &Expr = scrutinee;
                        self.results.derefs.insert(NodeKey::of(scrutinee), layers);
                        st = cur;
                    }
                }
                let mut result: Option<Ty> = None;
                for arm in arms {
                    self.push_scope();
                    self.pattern(&arm.pat, &st, false);
                    if let Some(g) = &arm.guard {
                        self.expr_coerce(g, &Ty::Bool);
                    }
                    let hint = result.clone().or_else(|| exp.cloned());
                    let bt = self.expr(&arm.body, hint.as_ref());
                    self.pop_scope();
                    result = Some(match result {
                        None => bt,
                        Some(r) => self.join(r, bt, arm.body.span),
                    });
                }
                self.check_exhaustive(&st, arms, scrutinee.span);
                result.unwrap_or(Ty::Never)
            }
            ExprKind::Loop(b) => {
                self.body.loops.push(LoopCtx {
                    break_ty: None,
                    is_loop: true,
                });
                self.block(b, None);
                let ctx = self.body.loops.pop().unwrap();
                ctx.break_ty.unwrap_or(Ty::Never)
            }
            ExprKind::While { cond, body } => {
                self.expr_coerce(cond, &Ty::Bool);
                self.body.loops.push(LoopCtx::default());
                self.block(body, None);
                self.body.loops.pop();
                Ty::unit()
            }
            ExprKind::For { pat, iter, guard, body, .. } => {
                self.for_loop(pat, iter, guard.as_deref(), body);
                Ty::unit()
            }
            ExprKind::Return(v) => {
                let target = self.body.ret.last().cloned().unwrap_or(Ty::unit());
                match v {
                    Some(x) => {
                        self.expr_coerce(x, &target);
                    }
                    None => self.coerce(&Ty::unit(), &target, e.span),
                }
                Ty::Never
            }
            ExprKind::Break(v) => {
                let vt = match v {
                    Some(x) => Some(self.expr(x, None)),
                    None => None,
                };
                if self.body.loops.is_empty() {
                    return self.err_ty(e.span, "`break` outside of a loop");
                }
                let is_loop = self.body.loops.last().unwrap().is_loop;
                if vt.is_some() && !is_loop {
                    self.err(e.span, "`break` with a value is only allowed in `loop`");
                }
                if is_loop {
                    let t = vt.unwrap_or(Ty::unit());
                    let prev = self.body.loops.last().unwrap().break_ty.clone();
                    match prev {
                        None => self.body.loops.last_mut().unwrap().break_ty = Some(t),
                        Some(p) => self.coerce(&t, &p, e.span),
                    }
                }
                Ty::Never
            }
            ExprKind::Continue => {
                if self.body.loops.is_empty() {
                    return self.err_ty(e.span, "`continue` outside of a loop");
                }
                Ty::Never
            }
            ExprKind::MutBorrow(inner) => {
                self.expr(inner, None);
                self.err_hint(
                    e.span,
                    "`mut` is only allowed on call arguments and `for` iterables",
                    "it marks a mutable borrow, like `scale(mut data, 2.0)`",
                );
                Ty::Error
            }
            // `own x` is `x`, moved
            ExprKind::Own(inner) => self.expr(inner, exp),
            ExprKind::ParReduce { op, pat, iter, body } => {
                let it = self.expr(iter, None);
                let elem = self.iter_elem(&it, iter.span);
                self.push_scope();
                self.pattern(pat, &elem, false);
                let bt = self.expr(body, exp);
                self.pop_scope();
                match op.name.as_str() {
                    "sum" | "prod" => self.require_trait(&bt, "Num", body.span),
                    "min" | "max" => self.require_trait(&bt, "Ord", body.span),
                    "any" | "all" => {
                        self.coerce(&bt, &Ty::Bool, body.span);
                        return Ty::Bool;
                    }
                    other => {
                        return self.err_ty(
                            op.span,
                            format!("unknown reduction `{other}`; use sum, prod, min, max, any or all"),
                        )
                    }
                }
                bt
            }
            ExprKind::Spawn(b) => {
                let r = self.fresh(e.span);
                let t = self.task_body(b, &r);
                match self.adt_named("Task") {
                    Some(task) => Ty::Adt(task, vec![t]),
                    None => Ty::Error,
                }
            }
            ExprKind::On { target, body } => {
                let tt = if matches!(&target.kind, ExprKind::Ident(n) if n == "local" && self.lookup_body_value("local").is_none() && self.lookup_local("local").is_none())
                {
                    Ty::Error
                } else {
                    self.expr(target, None)
                };
                if let (Some(c), false) = (self.adt_named("Cluster"), tt.is_error()) {
                    self.coerce(&tt, &Ty::Adt(c, vec![]), target.span);
                }
                let r = self.fresh(e.span);
                self.task_body(body, &r)
            }
        }
    }

    /// A block that runs elsewhere (`spawn`, `on`): `return` leaves the block, and
    /// loops outside are not visible.
    fn task_body(&mut self, b: &'a Block, ret: &Ty) -> Ty {
        let saved = std::mem::take(&mut self.body.loops);
        self.body.ret.push(ret.clone());
        let t = self.block(b, None);
        self.body.ret.pop();
        self.body.loops = saved;
        let span = match b.stmts.last() {
            Some(Stmt::Expr(e)) => e.span,
            _ => b.span,
        };
        self.coerce(&t, ret, span);
        ret.clone()
    }

    fn require_trait(&mut self, ty: &Ty, trait_name: &str, span: Span) {
        if let Some(tr) = self.prog.find_trait(trait_name) {
            self.body.obligations.push(Obligation {
                ty: ty.clone(),
                trait_: tr,
                args: vec![],
                span,
            });
        }
    }

    // ------------------------------------------------------------ names

    /// A name used as a value (not directly called).
    fn ident_value(&mut self, e: &'a Expr, name: &str, explicit: Option<&[Ty]>) -> Ty {
        if explicit.is_none() {
            if let Some(id) = self.lookup_local(name) {
                self.record_res(e, Res::Local(id));
                return self.body.locals[id as usize].ty.clone();
            }
        }
        match self.lookup_body_value(name) {
            Some(ValueItem::Fn(fid)) => {
                let inst = self.instantiate(fid, &[], explicit, e.span);
                self.record_res(e, Res::Fn(fid, inst.subst.clone()));
                Ty::Fn(inst.params.into_iter().map(|p| p.2).collect(), Box::new(inst.ret))
            }
            Some(ValueItem::Const(cid)) => {
                self.record_res(e, Res::Const(cid));
                self.prog.consts[cid.idx()].ty.clone()
            }
            Some(ValueItem::StructCtor(adt)) => {
                let args = match explicit {
                    Some(a) => a.to_vec(),
                    None => self.fresh_adt_args(adt, e.span),
                };
                let fields = self.struct_fields(adt, &args);
                self.record_res(e, Res::StructCtor(adt, args.clone()));
                Ty::Fn(fields, Box::new(Ty::Adt(adt, args)))
            }
            Some(ValueItem::Variant(adt, i)) => {
                let args = match explicit {
                    Some(a) => a.to_vec(),
                    None => self.fresh_adt_args(adt, e.span),
                };
                let fields = self.variant_fields(adt, i, &args);
                self.record_res(e, Res::Variant(adt, i, args.clone()));
                let result = Ty::Adt(adt, args);
                if fields.is_empty() {
                    result
                } else {
                    Ty::Fn(fields, Box::new(result))
                }
            }
            None => {
                if name == "local" {
                    return match self.adt_named("Cluster") {
                        Some(c) => Ty::Adt(c, vec![]),
                        None => Ty::Error,
                    };
                }
                let cx = self.body.cx.clone();
                let hint = if self.lookup_type(cx.module, name).is_some() {
                    format!("`{name}` is a type, not a value; to call a function on it write `{name}.function()`")
                } else if self.scopes[cx.module].modules.contains_key(name) {
                    format!("`{name}` is a module; write `{name}.item`")
                } else {
                    "check the spelling, or declare it with `let`".to_string()
                };
                self.err_hint(e.span, format!("cannot find value `{name}` in this scope"), hint);
                Ty::Error
            }
        }
    }

    fn struct_fields(&self, adt: AdtId, args: &[Ty]) -> Vec<Ty> {
        let subst: Vec<(GenericId, Ty)> = self.prog.adt(adt).generics.iter().copied().zip(args.iter().cloned()).collect();
        match &self.prog.adt(adt).kind {
            AdtKind::Struct(fs) => fs.iter().map(|f| f.ty.subst(&subst)).collect(),
            _ => vec![],
        }
    }

    fn variant_fields(&self, adt: AdtId, i: usize, args: &[Ty]) -> Vec<Ty> {
        let subst: Vec<(GenericId, Ty)> = self.prog.adt(adt).generics.iter().copied().zip(args.iter().cloned()).collect();
        match &self.prog.adt(adt).kind {
            AdtKind::Enum(vs) => vs[i].fields.iter().map(|t| t.subst(&subst)).collect(),
            _ => vec![],
        }
    }

    /// If `e` names a module, a type or an enum (rather than a value), say which.
    fn resolve_head(&mut self, e: &Expr) -> Option<Head> {
        let cx = self.body.cx.clone();
        match &e.kind {
            ExprKind::Ident(name) => {
                if self.lookup_local(name).is_some() {
                    return None;
                }
                if self.body.local_fns.iter().any(|s| s.contains_key(name.as_str())) {
                    return None;
                }
                if let Some((_, g)) = cx.generics.iter().rev().find(|(n, _)| n == name) {
                    return Some(Head::Type(Ty::Param(*g)));
                }
                if name == "Self" {
                    return cx.self_ty.clone().map(Head::Type);
                }
                if let Some(&m) = self.scopes[cx.module].modules.get(name.as_str()) {
                    return Some(Head::Module(m));
                }
                if let Some(p) = prim_type(name) {
                    return Some(Head::Type(p));
                }
                match self.lookup_type(cx.module, name) {
                    Some(TypeItem::Adt(id)) => Some(self.adt_head(id, e.span)),
                    Some(TypeItem::Alias(_)) => {
                        let id = Ident {
                            name: name.clone(),
                            span: e.span,
                        };
                        let t = self.lower_path_ty(&[id], None, e.span, &cx, true);
                        Some(Head::Type(t))
                    }
                    _ => None,
                }
            }
            ExprKind::TypeApp { name, args } => {
                let t = self.lower_path_ty(&[name.clone()], Some(args), e.span, &cx, true);
                Some(Head::Type(t))
            }
            ExprKind::Field { base, name } => {
                let Head::Module(m) = self.resolve_head(base)? else { return None };
                let from = self.body.cx.module;
                match self.type_in(from, m, &name.name) {
                    Ok(Some(TypeItem::Adt(id))) => return Some(self.adt_head(id, e.span)),
                    Err(()) => {
                        self.private_item(name.span, &name.name, m);
                        return Some(Head::Type(Ty::Error));
                    }
                    _ => {}
                }
                let path = format!("{}.{}", self.mods[m].0, name.name);
                self.mod_by_path.get(&path).map(|&sub| Head::Module(sub))
            }
            _ => None,
        }
    }

    /// `module.name` used as a value from the code being checked. `Err` when the item is private (the
    /// error is reported, and `args`, if any, are still checked so that their own errors show).
    fn module_value_or_private(&mut self, m: usize, name: &Ident, args: &'a [Expr]) -> Result<Option<ValueItem>, ()> {
        let from = self.body.cx.module;
        match self.value_in(from, m, &name.name) {
            Err(()) => {
                self.private_item(name.span, &name.name, m);
                for a in args {
                    self.expr(a, None);
                }
                Err(())
            }
            r => r,
        }
    }

    fn adt_head(&mut self, id: AdtId, span: Span) -> Head {
        let args = self.fresh_adt_args(id, span);
        if matches!(self.prog.adt(id).kind, AdtKind::Enum(_)) {
            Head::Enum(id, args)
        } else {
            Head::Type(Ty::Adt(id, args))
        }
    }

    // ------------------------------------------------------------ calls

    fn check_unsafe_call(&mut self, fid: FnId, span: Span) {
        if self.prog.fn_(fid).is_unsafe && self.body.unsafe_depth == 0 {
            let name = self.prog.fn_(fid).name.clone();
            self.err_hint(
                span,
                format!("call to unsafe function `{name}` requires an `unsafe` block"),
                "wrap the call in `unsafe { ... }`",
            );
        }
    }

    /// Check call arguments against parameters, including call-site `mut` markers.
    fn check_args(&mut self, args: &'a [Expr], params: &[(String, ParamMode, Ty)], span: Span, what: &str) {
        if args.len() != params.len() {
            let s = if params.len() == 1 { "" } else { "s" };
            self.err(
                span,
                format!("{what} takes {} argument{s}, but {} {} supplied", params.len(), args.len(), if args.len() == 1 { "was" } else { "were" }),
            );
        }
        // closures last, so their parameter types can be inferred from other arguments
        let order: Vec<usize> = (0..args.len())
            .filter(|&i| !matches!(args[i].kind, ExprKind::Closure { .. }))
            .chain((0..args.len()).filter(|&i| matches!(args[i].kind, ExprKind::Closure { .. })))
            .collect();
        for i in order {
            let arg = &args[i];
            let param = params.get(i);
            let ty = param.map(|p| p.2.clone());
            let mode = param.map(|p| p.1);
            if let ExprKind::MutBorrow(inner) = &arg.kind {
                match (mode, &ty) {
                    (Some(ParamMode::Mut), Some(t)) => {
                        self.arg_coerce(inner, t, ParamMode::Mut);
                        self.check_mutable_place(inner, "borrow as mutable");
                        let t = t.clone();
                        self.record(arg, &t);
                    }
                    (Some(_), Some(t)) => {
                        let pname = &param.unwrap().0;
                        self.err_hint(
                            arg.span,
                            format!("unexpected `mut`: parameter `{pname}` does not take a mutable borrow"),
                            "remove the `mut`",
                        );
                        self.expr_coerce(inner, t);
                    }
                    _ => {
                        self.expr(inner, None);
                    }
                }
                continue;
            }
            match (&ty, mode) {
                (Some(t), Some(m)) => {
                    if m == ParamMode::Mut {
                        let pname = &param.unwrap().0;
                        self.err_hint(
                            arg.span,
                            format!("argument for `mut` parameter `{pname}` must be marked `mut`"),
                            "write `mut` before the argument, like `f(mut x)`",
                        );
                    }
                    self.arg_coerce(arg, t, m);
                }
                _ => {
                    self.expr(arg, None);
                }
            }
        }
    }

    /// Does looking through `layers` boxes of `ty` pass through a `Shared`? (Its contents are read-only.)
    fn passes_shared(&self, ty: &Ty, layers: u32) -> bool {
        let mut cur = self.infer.shallow(ty);
        for _ in 0..layers {
            if self.is_shared(&cur) {
                return true;
            }
            match self.box_inner(&cur) {
                Some(i) => cur = self.infer.shallow(&i),
                None => return false,
            }
        }
        false
    }

    fn shared_is_read_only(&mut self, span: Span) {
        self.err_hint(span, "cannot change what is inside a `Shared`: shared data is read-only", "keep data that changes in a `Mutex`: `Shared<Mutex<T>>`");
    }

    /// Check an argument against its parameter's type. A box passed to a borrowed parameter of the
    /// type it holds is looked through (`eval(left)` with `left: Box<Expr>` and `fn eval(e: Expr)`).
    fn arg_coerce(&mut self, e: &'a Expr, expected: &Ty, mode: ParamMode) -> Ty {
        let t = self.expr(e, Some(expected));
        if mode != ParamMode::Own {
            let want = self.infer.shallow(expected);
            if self.box_inner(&want).is_none() && !matches!(want, Ty::Var(_) | Ty::Param(_) | Ty::Error) {
                let mut cur = self.infer.shallow(&t);
                let mut n = 0u32;
                while let Some(inner) = self.box_inner(&cur) {
                    cur = self.infer.shallow(&inner);
                    n += 1;
                }
                if n > 0 {
                    if mode == ParamMode::Mut && self.passes_shared(&t, n) {
                        self.shared_is_read_only(e.span);
                    }
                    self.results.derefs.insert(NodeKey::of(e), n);
                    self.coerce(&cur, expected, e.span);
                    return t;
                }
            }
        }
        self.coerce(&t, expected, e.span);
        t
    }

    /// Call a known function. `node` is where the resolution is recorded: the callee
    /// path for `f(x)` (`is_callee`), or the method-call expression itself for
    /// `T.f(x)` / `v.f(x)`.
    fn call_function(
        &mut self,
        node: &'a Expr,
        span: Span,
        fid: FnId,
        parent: &[(GenericId, Ty)],
        explicit: Option<&[Ty]>,
        args: &'a [Expr],
        is_callee: bool,
    ) -> Ty {
        self.check_unsafe_call(fid, span);
        let inst = self.instantiate(fid, parent, explicit, span);
        if inst.self_mode.is_some() {
            let name = self.prog.fn_(fid).name.clone();
            self.err_hint(
                span,
                format!("`{name}` is a method and needs a receiver"),
                format!("call it as `value.{name}(...)`"),
            );
        }
        let name = format!("`{}`", self.prog.fn_(fid).name);
        self.check_args(args, &inst.params, span, &name);
        let tys = inst.subst.clone();
        self.record_res(node, Res::Fn(fid, tys));
        if is_callee {
            let fn_ty = Ty::Fn(inst.params.iter().map(|p| p.2.clone()).collect(), Box::new(inst.ret.clone()));
            self.record(node, &fn_ty);
        }
        inst.ret
    }

    fn call_ctor(
        &mut self,
        node: &'a Expr,
        span: Span,
        adt: AdtId,
        variant: Option<usize>,
        targs: Vec<Ty>,
        args: &'a [Expr],
        is_callee: bool,
    ) -> Ty {
        let (fields, name) = match variant {
            Some(i) => {
                let n = match &self.prog.adt(adt).kind {
                    AdtKind::Enum(vs) => vs[i].name.clone(),
                    _ => String::new(),
                };
                (self.variant_fields(adt, i, &targs), n)
            }
            None => (self.struct_fields(adt, &targs), self.prog.adt(adt).name.clone()),
        };
        if variant.is_none() && self.prog.adt(adt).module != self.body.cx.module {
            // building a struct from outside needs every field to be public
            let private = match &self.prog.adt(adt).kind {
                AdtKind::Struct(fs) => fs.iter().find(|f| !f.public).map(|f| f.name.clone()),
                _ => None,
            };
            if let Some(f) = private {
                self.err_hint(span, format!("cannot build `{name}` here: its field `{f}` is private"), "the module that defines it can offer a `pub fn` that builds it, or the field can be made `pub`");
            }
        }
        let params: Vec<(String, ParamMode, Ty)> = fields
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, t)| (format!("field {i}"), ParamMode::Borrow, t))
            .collect();
        self.check_args(args, &params, span, &format!("`{name}`"));
        match variant {
            Some(i) => self.record_res(node, Res::Variant(adt, i, targs.clone())),
            None => self.record_res(node, Res::StructCtor(adt, targs.clone())),
        }
        let result = Ty::Adt(adt, targs);
        if is_callee {
            self.record(node, &Ty::Fn(fields, Box::new(result.clone())));
        }
        result
    }

    fn call(&mut self, e: &'a Expr, callee: &'a Expr, args: &'a [Expr]) -> Ty {
        // a path that names a function or constructor
        let (name, explicit): (Option<&str>, Option<Vec<Ty>>) = match &callee.kind {
            ExprKind::Ident(n) if self.lookup_local(n).is_none() => (Some(n.as_str()), None),
            ExprKind::TypeApp { name, args: targs } => {
                let cx = self.body.cx.clone();
                let tys = targs.iter().map(|a| self.lower_ty(a, &cx)).collect();
                (Some(name.name.as_str()), Some(tys))
            }
            _ => (None, None),
        };
        if let Some(name) = name {
            match self.lookup_body_value(name) {
                Some(ValueItem::Fn(fid)) => {
                    return self.call_function(callee, e.span, fid, &[], explicit.as_deref(), args, true);
                }
                Some(ValueItem::StructCtor(adt)) => {
                    let targs = explicit.unwrap_or_else(|| self.fresh_adt_args(adt, e.span));
                    return self.call_ctor(callee, e.span, adt, None, targs, args, true);
                }
                Some(ValueItem::Variant(adt, i)) => {
                    let targs = explicit.unwrap_or_else(|| self.fresh_adt_args(adt, e.span));
                    return self.call_ctor(callee, e.span, adt, Some(i), targs, args, true);
                }
                _ => {}
            }
        }
        // calling a value of function type
        let ct = self.expr(callee, None);
        let st = self.infer.shallow(&ct);
        match st {
            Ty::Fn(ps, r) => {
                let params: Vec<(String, ParamMode, Ty)> = ps
                    .into_iter()
                    .enumerate()
                    .map(|(i, t)| (format!("arg {i}"), ParamMode::Borrow, t))
                    .collect();
                self.check_args(args, &params, e.span, "this function");
                *r
            }
            Ty::Var(_) => {
                let ps: Vec<Ty> = args.iter().map(|a| self.expr(a, None)).collect();
                let r = self.fresh(e.span);
                self.infer.unify(&st, &Ty::Fn(ps, Box::new(r.clone())));
                r
            }
            Ty::Error => {
                for a in args {
                    self.expr(a, None);
                }
                Ty::Error
            }
            other => {
                for a in args {
                    self.expr(a, None);
                }
                let t = self.show(&other);
                self.err_ty(callee.span, format!("expected a function, found `{t}`"))
            }
        }
    }

    fn method_call(&mut self, e: &'a Expr, recv: &'a Expr, name: &Ident, args: &'a [Expr]) -> Ty {
        if let Some(head) = self.resolve_head(recv) {
            return self.head_call(e, head, name, args);
        }
        let rt = self.expr(recv, None);
        let mut st = self.infer.shallow(&rt);
        if let Ty::Var(v) = &st {
            match self.infer.kind_of(*v) {
                VarKind::Int => {
                    self.infer.unify(&st, &Ty::Int(IntTy::I64));
                }
                VarKind::Float => {
                    self.infer.unify(&st, &Ty::Float(FloatTy::F64));
                }
                VarKind::General => {
                    for a in args {
                        self.expr(a, None);
                    }
                    return self.err_ty(
                        recv.span,
                        format!("cannot call `.{}()` here: the type of the receiver is not known yet", name.name),
                    );
                }
            }
            st = self.infer.shallow(&st);
        }
        if st.is_error() {
            for a in args {
                self.expr(a, None);
            }
            return Ty::Error;
        }
        if let Ty::Tensor(elem, dims) = &st {
            if let Some(t) = self.tensor_method(e, recv, name, args, elem, dims) {
                return t;
            }
        }
        let Some(pick) = self.lookup_method(&st, &name.name, true, e.span) else {
            // `x.clone()` on any type that can be cloned
            if name.name == "clone" && args.is_empty() {
                if let Some(tr) = self.prog.find_trait("Clone") {
                    if !matches!(st, Ty::Var(_) | Ty::Error) && self.implements(&st, tr, &[]) {
                        let tdef = self.prog.trait_(tr).clone();
                        if let Some((_, fid)) = tdef.methods.first() {
                            self.record_res(e, Res::Fn(*fid, vec![(tdef.self_generic, st.clone())]));
                            return st;
                        }
                    }
                }
            }
            for a in args {
                self.expr(a, None);
            }
            let t = self.show(&st);
            let hint = if self.lookup_method(&st, &name.name, false, e.span).is_some() {
                Some(format!("`{}` is an associated function: call it as `{t}.{}(...)`", name.name, name.name))
            } else {
                None
            };
            let msg = format!("no method named `{}` found for type `{t}`", name.name);
            match hint {
                Some(h) => self.err_hint(name.span, msg, h),
                None => self.err(name.span, msg),
            }
            return Ty::Error;
        };
        self.check_unsafe_call(pick.fn_id, e.span);
        self.check_method_visible(pick.fn_id, name.span);
        if pick.boxes > 0 {
            self.results.derefs.insert(NodeKey::of(recv), pick.boxes);
        }
        let inst = self.instantiate(pick.fn_id, &pick.subst, None, e.span);
        if inst.self_mode == Some(ParamMode::Mut) {
            self.check_mutable_place(recv, "borrow as mutable");
            if pick.boxes > 0 && self.passes_shared(&st, pick.boxes) {
                self.shared_is_read_only(e.span);
            }
        }
        let what = format!("method `{}`", name.name);
        self.check_args(args, &inst.params, e.span, &what);
        let tys = inst.subst.clone();
        self.record_res(e, Res::Fn(pick.fn_id, tys));
        inst.ret
    }

    fn head_call(&mut self, e: &'a Expr, head: Head, name: &Ident, args: &'a [Expr]) -> Ty {
        match head {
            Head::Module(m) => match self.module_value_or_private(m, name, args) {
                Err(()) => Ty::Error,
                Ok(Some(ValueItem::Fn(fid))) => self.call_function(e, e.span, fid, &[], None, args, false),
                Ok(Some(ValueItem::StructCtor(adt))) => {
                    let targs = self.fresh_adt_args(adt, e.span);
                    self.call_ctor(e, e.span, adt, None, targs, args, false)
                }
                Ok(Some(ValueItem::Variant(adt, i))) => {
                    let targs = self.fresh_adt_args(adt, e.span);
                    self.call_ctor(e, e.span, adt, Some(i), targs, args, false)
                }
                _ => {
                    for a in args {
                        self.expr(a, None);
                    }
                    let mname = self.mods[m].0.clone();
                    self.err_ty(name.span, format!("module `{mname}` has no function `{}`", name.name))
                }
            },
            Head::Enum(adt, targs) => {
                if let AdtKind::Enum(vs) = &self.prog.adt(adt).kind {
                    if let Some(i) = vs.iter().position(|v| v.name == name.name) {
                        return self.call_ctor(e, e.span, adt, Some(i), targs, args, false);
                    }
                }
                self.assoc_call(e, Ty::Adt(adt, targs), name, args)
            }
            Head::Type(ty) => self.assoc_call(e, ty, name, args),
        }
    }

    /// The tensor constructors beyond `zeros`, `ones` and `random`:
    /// `from(xs)`, `from_rows(rows)`, `eye(n)`, `full(value, sizes...)`, `linspace(a, b, n)`, `arange(n)`.
    fn tensor_constructor(&mut self, e: &'a Expr, ty: &Ty, elem: &Ty, dims: &[Dim], name: &Ident, args: &'a [Expr]) -> Option<Ty> {
        let kind: &'static str = match name.name.as_str() {
            "from" => "tensor.from",
            "from_rows" => "tensor.from_rows",
            "eye" => "tensor.eye",
            "full" => "tensor.full",
            "linspace" => "tensor.linspace",
            "arange" => "tensor.arange",
            _ => return None,
        };
        let rank = dims.len();
        let want_rank = match kind {
            "tensor.full" => None,
            "tensor.from_rows" | "tensor.eye" => Some(2),
            _ => Some(1),
        };
        if let Some(r) = want_rank {
            if rank != r {
                self.err_hint(
                    e.span,
                    format!("`{}` makes a tensor of rank {r}, but this type has rank {rank}", name.name),
                    if r == 1 { "like `Tensor<f64, [_]>`" } else { "like `Tensor<f64, [_, _]>`" },
                );
            }
        }
        let float_only = kind == "tensor.linspace";
        if float_only && !matches!(elem, Ty::Float(_)) && !matches!(elem, Ty::Var(_) | Ty::Error) {
            self.err(e.span, format!("`{}` needs a tensor of floats", name.name));
        }
        if kind == "tensor.arange" && !matches!(elem, Ty::Float(_) | Ty::Int(_) | Ty::Var(_) | Ty::Error) {
            self.err(e.span, "`arange` needs a tensor of floats or integers");
        }
        let usz = Ty::usize();
        let slice = Ty::Slice(Box::new(elem.clone()));
        let rows = self.adt_named("Vec").map(|v| Ty::Adt(v, vec![Ty::Adt(v, vec![elem.clone()])])).unwrap_or(Ty::Error);
        let params: Vec<Ty> = match kind {
            "tensor.from" => vec![slice],
            "tensor.from_rows" => vec![rows],
            "tensor.eye" | "tensor.arange" => vec![usz.clone()],
            "tensor.linspace" => vec![elem.clone(), elem.clone(), usz.clone()],
            _ => std::iter::once(elem.clone()).chain((0..rank).map(|_| usz.clone())).collect(),
        };
        if args.len() != params.len() {
            let s = if params.len() == 1 { "" } else { "s" };
            self.err(e.span, format!("`{}` takes {} argument{s}, but {} {} supplied", name.name, params.len(), args.len(), if args.len() == 1 { "was" } else { "were" }));
        }
        for (a, p) in args.iter().zip(&params) {
            self.expr_coerce(a, p);
        }
        for a in args.iter().skip(params.len()) {
            self.expr(a, None);
        }
        self.record_res(e, Res::Builtin(kind));
        Some(ty.clone())
    }

    /// Methods of every tensor, whatever its rank: `sum`, `mean`, `min`, `max`, `shape`, `to_vec`, `map`,
    /// `reshape(sizes...)`, `transpose`; for `bool` tensors `count`, `any`, `all`; `select(mask)`,
    /// `to_f64` and `to_i64`.
    fn tensor_method(&mut self, e: &'a Expr, _recv: &'a Expr, name: &Ident, args: &'a [Expr], elem: &Ty, dims: &[Dim]) -> Option<Ty> {
        let kind: &'static str = match name.name.as_str() {
            "sum" => "tensor.sum",
            "mean" => "tensor.mean",
            "min" => "tensor.min",
            "max" => "tensor.max",
            "shape" => "tensor.shape",
            "to_vec" => "tensor.to_vec",
            "map" => "tensor.map",
            "reshape" => "tensor.reshape",
            "transpose" => "tensor.transpose",
            "count" => "tensor.count",
            "any" => "tensor.any",
            "all" => "tensor.all",
            "select" => "tensor.select",
            "to_f64" => "tensor.to_f64",
            "to_i64" => "tensor.to_i64",
            _ => return None,
        };
        let arity: Option<usize> = match kind {
            "tensor.map" | "tensor.select" => Some(1),
            "tensor.reshape" => None,
            _ => Some(0),
        };
        if let Some(n) = arity {
            if args.len() != n {
                let s = if n == 1 { "" } else { "s" };
                self.err(e.span, format!("`{}` takes {n} argument{s}, but {} {} supplied", name.name, args.len(), if args.len() == 1 { "was" } else { "were" }));
            }
        }
        let unknown = matches!(elem, Ty::Var(_) | Ty::Error | Ty::Param(_));
        let is_float = matches!(elem, Ty::Float(_));
        let is_int = matches!(elem, Ty::Int(_));
        let is_bool = matches!(elem, Ty::Bool);
        let is_complex = matches!(elem, Ty::Complex(_));
        match kind {
            "tensor.sum" if !unknown && !(is_float || is_complex || is_int) => {
                self.err_hint(e.span, "`sum` needs a tensor of numbers", "for a `bool` tensor, `count()` gives the number of `true`s");
            }
            "tensor.mean" if !unknown && !(is_float || is_complex) => {
                self.err_hint(e.span, "`mean` needs a tensor of floats or complex numbers", "convert an integer tensor first: `t.to_f64().mean()`");
            }
            "tensor.min" | "tensor.max" if !unknown && !(is_float || is_int) => {
                self.err(e.span, format!("`{}` needs a tensor of floats or integers (complex numbers and `bool` have no order here)", name.name));
            }
            "tensor.count" | "tensor.any" | "tensor.all" if !unknown && !is_bool => {
                self.err_hint(e.span, format!("`{}` needs a tensor of `bool`", name.name), "make one by comparing: `t > 0`");
            }
            "tensor.to_f64" if !unknown && is_complex => {
                self.err(e.span, "a complex tensor cannot be converted to `f64`");
            }
            "tensor.to_i64" if !unknown && is_complex => {
                self.err(e.span, "a complex tensor cannot be converted to `i64`");
            }
            "tensor.transpose" if dims.len() != 2 => {
                self.err(e.span, "`transpose` needs a tensor of rank 2");
            }
            _ => {}
        }
        let result = match kind {
            "tensor.sum" | "tensor.mean" | "tensor.min" | "tensor.max" => elem.clone(),
            "tensor.count" => Ty::usize(),
            "tensor.any" | "tensor.all" => Ty::Bool,
            "tensor.shape" => self.adt_named("Vec").map(|v| Ty::Adt(v, vec![Ty::usize()])).unwrap_or(Ty::Error),
            "tensor.to_vec" => self.adt_named("Vec").map(|v| Ty::Adt(v, vec![elem.clone()])).unwrap_or(Ty::Error),
            "tensor.to_f64" => Ty::Tensor(Box::new(Ty::f64()), dims.to_vec()),
            "tensor.to_i64" => Ty::Tensor(Box::new(Ty::Int(IntTy::I64)), dims.to_vec()),
            "tensor.map" => {
                let f = Ty::Fn(vec![elem.clone()], Box::new(elem.clone()));
                for a in args {
                    self.expr_coerce(a, &f);
                }
                Ty::Tensor(Box::new(elem.clone()), dims.to_vec())
            }
            "tensor.select" => {
                // the mask has the shape of the tensor; the result is the chosen elements as a vector
                let mask = Ty::Tensor(Box::new(Ty::Bool), dims.to_vec());
                for a in args {
                    let t = self.expr(a, Some(&mask));
                    let st = self.infer.shallow(&t);
                    match &st {
                        Ty::Tensor(me, _) if matches!(self.infer.shallow(me), Ty::Bool | Ty::Var(_) | Ty::Error) => {}
                        Ty::Error => {}
                        other => {
                            let shown = self.show(other);
                            self.err_hint(a.span, format!("`select` needs a mask: a tensor of `bool`, found `{shown}`"), "make one by comparing: `t.select(t > 0)`");
                        }
                    }
                }
                Ty::Tensor(Box::new(elem.clone()), vec![Dim::Dyn])
            }
            "tensor.reshape" => {
                if args.is_empty() {
                    self.err_hint(e.span, "`reshape` needs the new sizes", "like `t.reshape(2, 3)`");
                }
                for a in args {
                    self.expr_coerce(a, &Ty::usize());
                }
                Ty::Tensor(Box::new(elem.clone()), vec![Dim::Dyn; args.len().max(1)])
            }
            _ => Ty::Tensor(Box::new(elem.clone()), dims.iter().rev().cloned().collect()),
        };
        if !matches!(kind, "tensor.map" | "tensor.reshape" | "tensor.select") {
            for a in args {
                self.expr(a, None);
            }
        }
        self.record_res(e, Res::Builtin(kind));
        Some(result)
    }

    /// `Type.function(args)`
    fn assoc_call(&mut self, e: &'a Expr, ty: Ty, name: &Ident, args: &'a [Expr]) -> Ty {
        if let Ty::Tensor(elem, dims) = &ty {
            if matches!(name.name.as_str(), "zeros" | "ones" | "random") {
                let all_fixed = dims.iter().all(|d| matches!(d, Dim::Fixed(_)));
                let ok = args.len() == dims.len() || (args.is_empty() && all_fixed);
                if !ok {
                    let n = dims.len();
                    self.err_hint(
                        e.span,
                        format!("`{}` needs {n} size argument(s) for this shape, or none if every dimension is fixed", name.name),
                        "like `Tensor<f64, [_, _]>.zeros(3, 4)`",
                    );
                }
                for a in args {
                    self.expr_coerce(a, &Ty::usize());
                }
                if name.name == "random" && !matches!(**elem, Ty::Float(_) | Ty::Var(_) | Ty::Error) {
                    self.err_hint(e.span, "`random` needs a tensor of floats", "make integers with `arange` or `zeros`, or convert a float tensor");
                }
                self.record_res(e, Res::Builtin(match name.name.as_str() {
                    "zeros" => "tensor.zeros",
                    "ones" => "tensor.ones",
                    _ => "tensor.random",
                }));
                return ty.clone();
            }
            if let Some(t) = self.tensor_constructor(e, &ty, elem, dims, name, args) {
                return t;
            }
        }
        let Some(pick) = self.lookup_method(&ty, &name.name, false, e.span) else {
            for a in args {
                self.expr(a, None);
            }
            let t = self.show(&ty);
            let hint = if self.lookup_method(&ty, &name.name, true, e.span).is_some() {
                Some(format!("`{}` is a method: call it on a value, like `x.{}(...)`", name.name, name.name))
            } else {
                None
            };
            let msg = format!("no function named `{}` found for type `{t}`", name.name);
            match hint {
                Some(h) => self.err_hint(name.span, msg, h),
                None => self.err(name.span, msg),
            }
            return Ty::Error;
        };
        self.check_method_visible(pick.fn_id, name.span);
        self.call_function(e, e.span, pick.fn_id, &pick.subst, None, args, false)
    }

    /// A method of an inherent `impl` is usable from other modules only if it is `pub` (a method of a
    /// trait is as visible as the trait).
    fn check_method_visible(&mut self, fid: FnId, span: Span) {
        let def = self.prog.fn_(fid);
        let Parent::Impl(i) = def.parent else { return };
        if self.prog.impl_(i).trait_ref.is_some() || def.public || def.module == self.body.cx.module {
            return;
        }
        let (name, module) = (def.name.clone(), def.module);
        let path = self.mods[module].0.clone();
        self.err_hint(span, format!("the method `{name}` is private to the module `{path}`"), "add `pub` to the method to use it from other modules");
    }

    // ------------------------------------------------------------ field and index

    fn field(&mut self, e: &'a Expr, base: &'a Expr, name: &Ident) -> Ty {
        if let Some(head) = self.resolve_head(base) {
            return match head {
                Head::Module(m) => match self.module_value_or_private(m, name, &[]) {
                    Err(()) => Ty::Error,
                    Ok(Some(ValueItem::Const(cid))) => {
                        self.record_res(e, Res::Const(cid));
                        self.prog.consts[cid.idx()].ty.clone()
                    }
                    Ok(Some(ValueItem::Fn(fid))) => {
                        let inst = self.instantiate(fid, &[], None, e.span);
                        self.record_res(e, Res::Fn(fid, inst.subst.clone()));
                        Ty::Fn(inst.params.into_iter().map(|p| p.2).collect(), Box::new(inst.ret))
                    }
                    _ => {
                        let mname = self.mods[m].0.clone();
                        self.err_ty(name.span, format!("module `{mname}` has no item `{}`", name.name))
                    }
                },
                Head::Enum(adt, targs) => {
                    if let AdtKind::Enum(vs) = &self.prog.adt(adt).kind {
                        if let Some(i) = vs.iter().position(|v| v.name == name.name) {
                            let fields = self.variant_fields(adt, i, &targs);
                            self.record_res(e, Res::Variant(adt, i, targs.clone()));
                            let result = Ty::Adt(adt, targs);
                            return if fields.is_empty() { result } else { Ty::Fn(fields, Box::new(result)) };
                        }
                    }
                    let t = self.prog.adt(adt).name.clone();
                    self.err_ty(name.span, format!("enum `{t}` has no variant `{}`", name.name))
                }
                Head::Type(t) => {
                    let s = self.show(&t);
                    self.err_ty(name.span, format!("`{s}` has no associated item `{}`; call functions as `{s}.{}(...)`", name.name, name.name))
                }
            };
        }
        let bt = self.expr(base, None);
        let mut st = self.infer.shallow(&bt);
        // a box is looked through to the value it holds
        let mut layers = 0u32;
        while let Some(inner) = self.box_inner(&st) {
            st = self.infer.shallow(&inner);
            layers += 1;
        }
        if layers > 0 {
            self.results.derefs.insert(NodeKey::of(base), layers);
        }
        match &st {
            Ty::Adt(id, args) => {
                if let AdtKind::Struct(fs) = &self.prog.adt(*id).kind {
                    if let Some(f) = fs.iter().find(|f| f.name == name.name) {
                        let subst: Vec<(GenericId, Ty)> =
                            self.prog.adt(*id).generics.iter().copied().zip(args.iter().cloned()).collect();
                        let (fty, public, owner) = (f.ty.subst(&subst), f.public, self.prog.adt(*id).module);
                        if !public && owner != self.body.cx.module {
                            let sname = self.prog.adt(*id).name.clone();
                            self.err_hint(name.span, format!("the field `{}` of `{sname}` is private", name.name), "add `pub` to the field in its declaration");
                        }
                        return fty;
                    }
                }
                let t = self.show(&st);
                if self.lookup_method(&st, &name.name, true, e.span).is_some() {
                    self.err_hint(
                        name.span,
                        format!("`{t}` has no field `{}`", name.name),
                        format!("`{}` is a method; call it with `.{}()`", name.name, name.name),
                    );
                    Ty::Error
                } else {
                    self.err_ty(name.span, format!("no field `{}` on type `{t}`", name.name))
                }
            }
            Ty::Tuple(ts) => match name.name.parse::<usize>() {
                Ok(i) if i < ts.len() => ts[i].clone(),
                _ => {
                    let t = self.show(&st);
                    self.err_ty(name.span, format!("no field `{}` on type `{t}`", name.name))
                }
            },
            Ty::Slice(_) | Ty::Array(..) | Ty::Str | Ty::Tensor(..) if name.name == "len" => Ty::usize(),
            Ty::Error => Ty::Error,
            Ty::Var(_) => self.err_ty(base.span, format!("cannot access `.{}` here: the type is not known yet", name.name)),
            other => {
                let t = self.show(other);
                self.err_ty(name.span, format!("no field `{}` on type `{t}`", name.name))
            }
        }
    }

    fn index(&mut self, e: &'a Expr, base: &'a Expr, indices: &'a [Expr]) -> Ty {
        // `vec[1, 2, 3]` builds a vector
        if let ExprKind::Ident(n) = &base.kind {
            if n == "vec" && self.lookup_local(n).is_none() && self.lookup_body_value(n).is_none() {
                let elem = self.fresh(e.span);
                for i in indices {
                    self.expr_coerce(i, &elem);
                }
                return match self.adt_named("Vec") {
                    Some(v) => Ty::Adt(v, vec![elem]),
                    None => Ty::Error,
                };
            }
        }
        let bt = self.expr(base, None);
        let mut st = self.infer.shallow(&bt);
        // a `Box` or `Shared` is looked through to the vector, array or tensor it holds
        let mut layers = 0u32;
        while let Some(inner) = self.box_inner(&st) {
            st = self.infer.shallow(&inner);
            layers += 1;
        }
        if layers > 0 {
            self.results.derefs.insert(NodeKey::of(base), layers);
        }
        let elem_of = |this: &Self, t: &Ty| -> Option<Ty> {
            match t {
                Ty::Array(x, _) | Ty::Slice(x) => Some((**x).clone()),
                _ => this.is_vec(t),
            }
        };
        if let Some(elem) = elem_of(self, &st) {
            if indices.len() != 1 {
                for i in indices {
                    self.expr(i, None);
                }
                return self.err_ty(e.span, "a vector or array takes exactly one index");
            }
            if matches!(indices[0].kind, ExprKind::Range { .. }) {
                let rt = self.expr(&indices[0], None);
                let _ = rt;
                return Ty::Slice(Box::new(elem));
            }
            self.expr_coerce(&indices[0], &Ty::usize());
            return elem;
        }
        match &st {
            Ty::Tensor(elem, dims) => {
                if indices.len() != dims.len() {
                    for i in indices {
                        self.expr(i, None);
                    }
                    return self.err_ty(
                        e.span,
                        format!("this tensor has {} dimension(s), but {} index(es) were given", dims.len(), indices.len()),
                    );
                }
                // each index is a position (`i`), or a range of positions (`a..b`, `a..=b`, `..b`, `a..`, `..`)
                // that keeps its axis: `t[1, 0..3]` is a tensor of rank 1, a copy of part of `t`
                let mut ranges = 0;
                for i in indices {
                    if let ExprKind::Range { start, end, .. } = &i.kind {
                        for x in [start, end].into_iter().flatten() {
                            self.expr_coerce(x, &Ty::usize());
                        }
                        if let Some(r) = self.adt_named("Range") {
                            self.record(i, &Ty::Adt(r, vec![Ty::usize()]));
                        }
                        ranges += 1;
                    } else {
                        self.expr_coerce(i, &Ty::usize());
                    }
                }
                if ranges == 0 {
                    (**elem).clone()
                } else {
                    Ty::Tensor(elem.clone(), vec![Dim::Dyn; ranges])
                }
            }
            Ty::Adt(..) => {
                if indices.len() != 1 {
                    return self.err_ty(e.span, "this type is indexed with exactly one index");
                }
                if matches!(&st, Ty::Adt(id, _) if Some(*id) == self.adt_named("Ptr")) && self.body.unsafe_depth == 0 {
                    self.err_hint(
                        e.span,
                        "dereferencing a raw pointer requires an `unsafe` block",
                        "wrap the access in `unsafe { ... }`",
                    );
                }
                let it = self.expr(&indices[0], None);
                match self.lookup_op("Index", &st, Some(&it), e.span) {
                    Some((fid, inst)) => {
                        let tys = inst.subst.clone();
                        self.record_res(e, Res::Fn(fid, tys));
                        inst.ret
                    }
                    None => {
                        let (t, i) = (self.show(&st), self.show(&it));
                        self.err_ty(e.span, format!("cannot index `{t}` with `{i}`"))
                    }
                }
            }
            Ty::Error => {
                for i in indices {
                    self.expr(i, None);
                }
                Ty::Error
            }
            Ty::Var(_) => self.err_ty(base.span, "cannot index here: the type is not known yet"),
            other => {
                for i in indices {
                    self.expr(i, None);
                }
                let t = self.show(other);
                self.err_ty(e.span, format!("cannot index into a value of type `{t}`"))
            }
        }
    }

    // ------------------------------------------------------------ `?`, closures, loops

    fn try_expr(&mut self, e: &'a Expr, inner: &'a Expr) -> Ty {
        let t = self.expr(inner, None);
        let st = self.infer.shallow(&t);
        let result = self.adt_named("Result");
        let option = self.adt_named("Option");
        let ret = self.body.ret.last().cloned().map(|r| self.infer.shallow(&r));
        match &st {
            Ty::Adt(id, args) if Some(*id) == result && args.len() == 2 => {
                match &ret {
                    Some(Ty::Adt(rid, rargs)) if Some(*rid) == result && rargs.len() == 2 => {
                        if !self.infer.unify(&args[1], &rargs[1]) {
                            let (a, b) = (self.show(&args[1]), self.show(&rargs[1]));
                            self.err_hint(
                                e.span,
                                format!("the `?` operator cannot convert the error `{a}` into `{b}`"),
                                "make the function's error type match",
                            );
                        }
                    }
                    Some(Ty::Error) | Some(Ty::Var(_)) => {}
                    _ => {
                        self.err_hint(e.span, "the `?` operator can only be used in a function that returns `Result`", "change the return type to `Result<_, _>`");
                    }
                }
                args[0].clone()
            }
            Ty::Adt(id, args) if Some(*id) == option && args.len() == 1 => {
                match &ret {
                    Some(Ty::Adt(rid, _)) if Some(*rid) == option => {}
                    Some(Ty::Error) | Some(Ty::Var(_)) => {}
                    _ => {
                        self.err_hint(e.span, "the `?` operator on an `Option` can only be used in a function that returns `Option`", "change the return type to `Option<_>`");
                    }
                }
                args[0].clone()
            }
            Ty::Error => Ty::Error,
            other => {
                let t = self.show(other);
                self.err_ty(e.span, format!("the `?` operator can only be applied to `Result` or `Option`, not `{t}`"))
            }
        }
    }

    fn closure(
        &mut self,
        e: &'a Expr,
        params: &'a [ClosureParam],
        ret: Option<&'a TypeExpr>,
        body: &'a Expr,
        exp: Option<&Ty>,
    ) -> Ty {
        let (exp_params, exp_ret) = match exp.map(|t| self.infer.shallow(t)) {
            Some(Ty::Fn(ps, r)) if ps.len() == params.len() => (Some(ps), Some(*r)),
            _ => (None, None),
        };
        let cx = self.body.cx.clone();
        self.push_scope();
        let mut ptys = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let ty = match (&p.ty, &exp_params) {
                (Some(t), Some(ep)) => {
                    let a = self.lower_ty(t, &cx);
                    self.coerce(&ep[i], &a, e.span);
                    a
                }
                (Some(t), None) => self.lower_ty(t, &cx),
                (None, Some(ep)) => ep[i].clone(),
                (None, None) => self.fresh(p.pat.span),
            };
            self.pattern(&p.pat, &ty, false);
            ptys.push(ty);
        }
        let rt = match (ret, exp_ret) {
            (Some(t), _) => self.lower_ty(t, &cx),
            (None, Some(r)) => r,
            (None, None) => self.fresh(e.span),
        };
        let saved = std::mem::take(&mut self.body.loops);
        self.body.ret.push(rt.clone());
        let bt = self.expr(body, Some(&rt));
        self.body.ret.pop();
        self.body.loops = saved;
        self.coerce(&bt, &rt, body.span);
        self.pop_scope();
        Ty::Fn(ptys, Box::new(rt))
    }

    /// The type of the items produced when iterating over `ty`.
    fn iter_elem(&mut self, ty: &Ty, span: Span) -> Ty {
        let st = self.infer.shallow(ty);
        match &st {
            Ty::Adt(id, args) if Some(*id) == self.adt_named("Range") && args.len() == 1 => {
                let t = self.infer.shallow(&args[0]);
                let float_literal = matches!(&t, Ty::Var(v) if self.infer.kind_of(*v) == VarKind::Float);
                if matches!(t, Ty::Float(_)) || float_literal {
                    return self.err_ty(span, "cannot iterate over a range of floats");
                }
                t
            }
            Ty::Array(t, _) | Ty::Slice(t) => (**t).clone(),
            Ty::Adt(..) => match self.is_vec(&st) {
                Some(t) => t,
                None => {
                    let s = self.show(&st);
                    self.err_hint(span, format!("`{s}` is not iterable"), "a type can be looped over when it has `impl Iterator<T> for ...` with a `next` method, or call `.iter()` on a collection");
                    Ty::Error
                }
            },
            Ty::Error => Ty::Error,
            Ty::Var(v) if self.infer.kind_of(*v) == VarKind::General => {
                self.err_ty(span, "cannot iterate here: the type is not known yet")
            }
            other => {
                let s = self.show(other);
                self.err_ty(span, format!("`{s}` is not iterable"))
            }
        }
    }

    /// A value with a `next` method from an `impl Iterator<T>` can be looped over: the items are `T`.
    /// Records which `next` to call.
    fn iterator_item(&mut self, st: &Ty, key: &'a Expr) -> Option<Ty> {
        let Ty::Adt(id, _) = st else { return None };
        if Some(*id) == self.adt_named("Range") || self.is_vec(st).is_some() {
            return None;
        }
        let tr = self.prog.find_trait("Iterator")?;
        let pick = self.lookup_method(st, "next", true, key.span)?;
        let is_iterator = match self.prog.fn_(pick.fn_id).parent {
            Parent::Impl(i) => self.prog.impl_(i).trait_ref.as_ref().map(|t| t.trait_) == Some(tr),
            Parent::Trait(t) => t == tr,
            Parent::Free => false,
        };
        if !is_iterator {
            return None;
        }
        let inst = self.instantiate(pick.fn_id, &pick.subst, None, key.span);
        let Ty::Adt(oid, oargs) = self.infer.shallow(&inst.ret) else { return None };
        if Some(oid) != self.adt_named("Option") || oargs.len() != 1 {
            return None;
        }
        self.results.for_next.insert(NodeKey::of(key), (pick.fn_id, inst.subst.clone()));
        self.body.touched_nexts.push(NodeKey::of(key));
        Some(oargs[0].clone())
    }

    fn for_loop(&mut self, pat: &'a Pattern, iter: &'a Expr, guard: Option<&'a Expr>, body: &'a Block) {
        let (iter_expr, mut_iter): (&'a Expr, bool) = match &iter.kind {
            ExprKind::MutBorrow(inner) => (inner, true),
            _ => (iter, false),
        };
        let it = self.expr(iter_expr, None);
        if mut_iter {
            self.check_mutable_place(iter_expr, "borrow as mutable");
            self.record(iter, &it);
        }
        // `par_chunks` hands out disjoint mutable slices
        let yields_mut = matches!(&iter_expr.kind, ExprKind::Call { callee, .. }
            if matches!(&callee.kind, ExprKind::Ident(n) if n == "par_chunks"));
        let st = self.infer.shallow(&it);
        let elem = match self.iterator_item(&st, iter) {
            Some(t) => t,
            None => self.iter_elem(&it, iter.span),
        };
        self.push_scope();
        self.pattern(pat, &elem, mut_iter || yields_mut);
        if let Some(g) = guard {
            self.expr_coerce(g, &Ty::Bool);
        }
        self.body.loops.push(LoopCtx::default());
        self.block(body, None);
        self.body.loops.pop();
        self.pop_scope();
    }

    // ------------------------------------------------------------ mutability

    /// Is this expression a raw pointer (`Ptr<T>`)? Writing through one does not
    /// require the pointer variable itself to be `mut`.
    fn is_ptr_expr(&self, e: &Expr) -> bool {
        match self.results.expr_ty.get(&NodeKey::of(e)) {
            Some(t) => matches!(self.infer.shallow(t), Ty::Adt(id, _) if Some(id) == self.adt_named("Ptr")),
            None => false,
        }
    }

    /// Check that `e` is a place that may be modified (assigned to, or borrowed `mut`).
    pub(super) fn check_mutable_place(&mut self, e: &Expr, action: &str) {
        match &e.kind {
            ExprKind::Ident(name) => match self.lookup_local(name) {
                Some(id) => {
                    if !self.body.locals[id as usize].mutable {
                        self.err_hint(
                            e.span,
                            format!("cannot {action} `{name}` because it is not declared `mut`"),
                            format!("declare it with `let mut {name}`, or as a `mut` parameter `{name}: mut T`"),
                        );
                    }
                }
                None => {
                    if matches!(self.lookup_body_value(name), Some(ValueItem::Const(_))) {
                        self.err(e.span, format!("cannot {action} the constant `{name}`"));
                    }
                }
            },
            ExprKind::Index { base, .. } if self.is_ptr_expr(base) => {}
            ExprKind::Field { base, .. } | ExprKind::Index { base, .. } => {
                let base: &Expr = base;
                if let (Some(bt), Some(n)) = (self.results.expr_ty.get(&NodeKey::of(base)).cloned(), self.results.derefs.get(&NodeKey::of(base)).copied()) {
                    if self.passes_shared(&bt, n) {
                        self.shared_is_read_only(e.span);
                    }
                }
                self.check_mutable_place(base, action)
            }
            ExprKind::MethodCall { .. } | ExprKind::Call { .. } => {
                // `v.get_mut()`-style results are not tracked; only direct places can be modified
                self.err(e.span, format!("cannot {action} a temporary value"));
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ operators

    /// Does the generic parameter carry a bound named `trait_name` (e.g. `Num`)?
    fn param_has_bound(&self, g: GenericId, trait_name: &str) -> bool {
        self.prog
            .generic(g)
            .bounds
            .iter()
            .any(|b| self.prog.trait_(b.trait_).name == trait_name && self.prog.trait_(b.trait_).module == 0)
    }

    /// Numbers, and generic parameters bounded by `Num` or `Float`/`Int`, support the
    /// arithmetic operators directly.
    fn is_numeric_like(&self, t: &Ty) -> bool {
        match t {
            Ty::Int(_) | Ty::Float(_) | Ty::Complex(_) => true,
            Ty::Var(v) => self.infer.kind_of(*v) != VarKind::General,
            Ty::Param(g) => ["Num", "Int", "Float"].iter().any(|n| self.param_has_bound(*g, n)),
            _ => false,
        }
    }

    fn is_int_like(&self, t: &Ty) -> bool {
        match t {
            Ty::Int(_) => true,
            Ty::Param(g) => self.param_has_bound(*g, "Int"),
            Ty::Var(v) => self.infer.kind_of(*v) == VarKind::Int,
            _ => false,
        }
    }

    fn unary(&mut self, e: &'a Expr, op: UnOp, inner: &'a Expr, exp: Option<&Ty>) -> Ty {
        let hint = if op == UnOp::Neg { self.numeric_hint(exp) } else { None };
        let t = self.expr(inner, hint.as_ref());
        if op == UnOp::Neg {
            // `-128i8` is fine although `128i8` alone is not
            if let Some(last) = self.body.int_lits.last_mut() {
                if last.0 == NodeKey::of(inner) {
                    last.3 = true;
                }
            }
        }
        let st = self.infer.shallow(&t);
        match op {
            UnOp::Not => {
                if let Ty::Tensor(el, _) = &st {
                    if matches!(self.infer.shallow(el), Ty::Bool | Ty::Var(_) | Ty::Error) {
                        return st;
                    }
                }
                self.coerce(&st, &Ty::Bool, inner.span);
                Ty::Bool
            }
            UnOp::BitNot => {
                if !self.is_int_like(&st) && !st.is_error() {
                    let s = self.show(&st);
                    return self.err_ty(e.span, format!("cannot apply `~` to `{s}`"));
                }
                st
            }
            UnOp::Neg => match &st {
                t if self.is_numeric_like(t) => st,
                Ty::Tensor(el, _) if matches!(self.infer.shallow(el), Ty::Bool) => {
                    return self.err_ty(e.span, "cannot negate a tensor of `bool`; use `!` for the opposite");
                }
                Ty::Tensor(..) | Ty::Error => st,
                Ty::Adt(..) | Ty::Param(_) => match self.lookup_op("Neg", &st, None, e.span) {
                    Some((fid, inst)) => {
                        let tys = inst.subst.clone();
                        self.record_res(e, Res::Fn(fid, tys));
                        inst.ret
                    }
                    None => {
                        let s = self.show(&st);
                        self.err_hint(e.span, format!("cannot negate `{s}`"), format!("implement `Neg` for `{s}`"));
                        Ty::Error
                    }
                },
                other => {
                    let s = self.show(other);
                    self.err_ty(e.span, format!("cannot negate `{s}`"))
                }
            },
        }
    }

    fn binary(&mut self, e: &'a Expr, op: BinOp, lhs: &'a Expr, rhs: &'a Expr, exp: Option<&Ty>) -> Ty {
        if matches!(op, BinOp::And | BinOp::Or) {
            self.expr_coerce(lhs, &Ty::Bool);
            self.expr_coerce(rhs, &Ty::Bool);
            return Ty::Bool;
        }
        let arithmetic = !matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge);
        let hint = if arithmetic { self.numeric_hint(exp) } else { None };
        let lt = self.expr(lhs, hint.as_ref());
        let rt = self.expr(rhs, Some(&lt));
        self.binary_result(e, op, &lt, &rt, lhs.span, rhs.span)
    }

    fn binary_result(&mut self, e: &'a Expr, op: BinOp, lt: &Ty, rt: &Ty, lspan: Span, rspan: Span) -> Ty {
        let l = self.infer.shallow(lt);
        let r = self.infer.shallow(rt);
        let comparison = matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge);
        if l.is_error() || r.is_error() {
            return if comparison { Ty::Bool } else { Ty::Error };
        }
        if matches!(l, Ty::Var(v) if self.infer.kind_of(v) == VarKind::General) {
            return self.err_ty(lspan, "cannot apply this operator: the type of the left operand is not known yet; add a type annotation");
        }

        // comparing tensors is element by element and gives a tensor of `bool`
        if comparison {
            let tensor_dims = |t: &Ty| if let Ty::Tensor(_, d) = t { Some(d.clone()) } else { None };
            if let Some(dims) = tensor_dims(&l).or_else(|| tensor_dims(&r)) {
                let elem_of = |t: &Ty| if let Ty::Tensor(e, _) = t { (**e).clone() } else { t.clone() };
                let (le, re) = (elem_of(&l), elem_of(&r));
                if !self.infer.unify(&le, &re) {
                    self.mismatch(rspan, &le, &re);
                    return Ty::Tensor(Box::new(Ty::Bool), dims);
                }
                let is_eq = matches!(op, BinOp::Eq | BinOp::Ne);
                let e_ = self.infer.shallow(&le);
                if !is_eq && matches!(e_, Ty::Complex(_) | Ty::Bool) {
                    let shown = self.show(&e_);
                    self.err(e.span, format!("`{}` cannot compare tensors of `{shown}` (only `==` and `!=`)", op.symbol()));
                }
                if matches!(e_, Ty::Tensor(..)) {
                    return self.err_ty(e.span, "cannot compare tensors of tensors");
                }
                if let (Ty::Tensor(_, ld), Ty::Tensor(_, rd)) = (&l, &r) {
                    let _ = (ld, rd); // shapes are checked when it runs
                }
                return Ty::Tensor(Box::new(Ty::Bool), dims);
            }
        }
        if comparison {
            let is_eq = matches!(op, BinOp::Eq | BinOp::Ne);
            let overloaded = matches!(&l, Ty::Adt(..)) && !self.is_string(&l) && self.is_vec(&l).is_none() && self.box_inner(&l).is_none();
            if overloaded {
                let tn = if is_eq { "Eq" } else { "Ord" };
                return match self.lookup_op(tn, &l, Some(&r), e.span) {
                    Some((fid, inst)) => {
                        let tys = inst.subst.clone();
                        self.record_res(e, Res::Fn(fid, tys));
                        Ty::Bool
                    }
                    None => {
                        let s = self.show(&l);
                        self.err_hint(e.span, format!("cannot compare `{s}` values with `{}`", op.symbol()), format!("implement `{tn}` for `{s}`"));
                        Ty::Bool
                    }
                };
            }
            if !self.infer.unify(&l, &r) {
                self.mismatch(rspan, &l, &r);
                return Ty::Bool;
            }
            self.require_trait(&l, if is_eq { "Eq" } else { "Ord" }, lspan);
            return Ty::Bool;
        }

        // tensors
        if let Ty::Tensor(le, ld) = &l {
            let el = self.infer.shallow(le);
            let arith = matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div);
            let logic = matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor);
            let int_only = matches!(op, BinOp::Rem);
            if matches!(el, Ty::Bool) && (arith || int_only) {
                self.err_hint(e.span, format!("`{}` does not work on tensors of `bool`", op.symbol()), "use `&`, `|` or `^`, or convert with `to_i64()` or `to_f64()`");
                return Ty::Error;
            }
            if (logic || int_only) && !matches!(el, Ty::Int(_) | Ty::Bool | Ty::Var(_) | Ty::Error | Ty::Param(_)) {
                let shown = self.show(&el);
                return self.err_ty(e.span, format!("`{}` only works on tensors of integers{}, not `{shown}`", op.symbol(), if logic { " and `bool`" } else { "" }));
            }
            match op {
                BinOp::MatMul => return self.matmul(e, &l, &r),
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                    return match &r {
                        Ty::Tensor(..) => {
                            if !self.infer.unify(&l, &r) {
                                self.mismatch(rspan, &l, &r);
                            }
                            l.clone()
                        }
                        _ => {
                            if !self.infer.unify(le, &r) {
                                self.mismatch(rspan, le, &r);
                            }
                            Ty::Tensor(le.clone(), ld.clone())
                        }
                    };
                }
                _ => {}
            }
        }
        if self.is_numeric_like(&l) {
            if let (Ty::Tensor(re, rd), BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor) = (&r, op) {
                if !self.infer.unify(&l, re) {
                    self.mismatch(lspan, re, &l);
                }
                return Ty::Tensor(re.clone(), rd.clone());
            }
            if matches!(op, BinOp::MatMul) {
                return self.err_ty(e.span, "`@` multiplies tensors; use `*` for numbers");
            }
            if !self.infer.unify(&l, &r) {
                self.mismatch(rspan, &l, &r);
                return l;
            }
            let bitwise = matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr);
            if bitwise && !self.is_int_like(&self.infer.shallow(&l)) {
                let s = self.show(&l);
                return self.err_ty(e.span, format!("`{}` only works on integers, not `{s}`", op.symbol()));
            }
            return l;
        }

        // overloaded operators on user types and generic parameters
        let tn = op_trait(op);
        if matches!(l, Ty::Adt(..) | Ty::Param(_)) {
            return match self.lookup_op(tn, &l, Some(&r), e.span) {
                Some((fid, inst)) => {
                    let tys = inst.subst.clone();
                    self.record_res(e, Res::Fn(fid, tys));
                    inst.ret
                }
                None => {
                    let (a, b) = (self.show(&l), self.show(&r));
                    self.err_hint(
                        e.span,
                        format!("cannot apply `{}` to `{a}` and `{b}`", op.symbol()),
                        format!("implement `{tn}` for `{a}` (with a method taking `{b}`)"),
                    );
                    Ty::Error
                }
            };
        }
        let (a, b) = (self.show(&l), self.show(&r));
        self.err_ty(e.span, format!("cannot apply `{}` to `{a}` and `{b}`", op.symbol()))
    }

    fn matmul(&mut self, e: &'a Expr, l: &Ty, r: &Ty) -> Ty {
        let (Ty::Tensor(le, ld), Ty::Tensor(re, rd)) = (l, r) else {
            let (a, b) = (self.show(l), self.show(r));
            return self.err_ty(e.span, format!("`@` needs two tensors, found `{a}` and `{b}`"));
        };
        if !self.infer.unify(le, re) {
            let (a, b) = (self.show(le), self.show(re));
            return self.err_ty(e.span, format!("cannot multiply tensors of `{a}` and `{b}` elements"));
        }
        let inner_ok = match (ld.last(), rd.first()) {
            (Some(Dim::Fixed(a)), Some(Dim::Fixed(b))) => a == b,
            _ => true,
        };
        match (ld.len(), rd.len()) {
            (2, 2) | (2, 1) | (1, 1) if inner_ok => match (ld.len(), rd.len()) {
                (2, 2) => Ty::Tensor(le.clone(), vec![ld[0].clone(), rd[1].clone()]),
                (2, 1) => Ty::Tensor(le.clone(), vec![ld[0].clone()]),
                _ => (**le).clone(),
            },
            (2, 2) | (2, 1) | (1, 1) => {
                let (a, b) = (self.show(l), self.show(r));
                self.err_ty(e.span, format!("shapes do not line up for `@`: `{a}` and `{b}`"))
            }
            (a, b) => self.err_ty(e.span, format!("`@` is defined for vectors and matrices, not for ranks {a} and {b}")),
        }
    }

    fn check_cast(&mut self, from: &Ty, to: &Ty, span: Span) {
        let f = self.infer.shallow(from);
        let t = self.infer.shallow(to);
        let scalar = |x: &Ty, this: &Self| matches!(x, Ty::Int(_) | Ty::Float(_) | Ty::Bool | Ty::Char) || this.is_numeric_like(x) && !matches!(x, Ty::Complex(_));
        let ok = match (&f, &t) {
            (Ty::Error, _) | (_, Ty::Error) => true,
            (Ty::Char, Ty::Int(i)) => !matches!(i, IntTy::I8),
            (Ty::Int(IntTy::U8), Ty::Char) => true,
            (_, Ty::Char) => false,
            (_, Ty::Bool) => false,
            (a, Ty::Int(_) | Ty::Float(_)) => scalar(a, self),
            (Ty::Complex(_), Ty::Complex(_)) => true,
            (a, Ty::Complex(_)) => scalar(a, self),
            _ => false,
        };
        if !ok {
            let (a, b) = (self.show(&f), self.show(&t));
            self.err(span, format!("cannot cast `{a}` to `{b}`"));
        }
    }
}

/// Is `lhs` a part of a tensor, like `t[0..2, 1]` (an index with at least one range)?
fn is_tensor_slice_target(lhs: &Expr) -> bool {
    matches!(&lhs.kind, ExprKind::Index { indices, .. } if indices.iter().any(|i| matches!(i.kind, ExprKind::Range { .. })) && indices.len() > 1)
        || matches!(&lhs.kind, ExprKind::Index { indices, .. } if indices.len() == 1 && matches!(indices[0].kind, ExprKind::Range { .. }))
}
