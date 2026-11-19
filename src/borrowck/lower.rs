//! Lowering typed AST to the event CFG (`mir`).
//!
//! The rules implemented here (all consequences of "borrow by default"):
//!  * Evaluating a place expression by value *moves* a non-`Copy` value (copies a `Copy`
//!    one). Moving out of a borrowed parameter, a view binding, or an index is an error.
//!  * Arguments for `Borrow` parameters are shared loans, `mut` arguments are mutable
//!    loans, `own` arguments are moves. Loans for one call are created just before the
//!    call (after the other arguments are evaluated), so `v.push(v.len)` is fine but
//!    `f(mut v, v)` is not.
//!  * A value of a *view* type (a slice, `str`, a task, a closure, or anything that
//!    contains one) carries the loans it was derived from; a local that holds it keeps
//!    those loans alive for as long as the local is used.

use std::collections::{HashMap, HashSet};

use super::mir::*;
use crate::ast::*;
use crate::check::{NodeKey, Res, Results};
use crate::diag::Diagnostic;
use crate::program::*;
use crate::span::Span;
use crate::types::*;

enum Operand {
    Place(Place, Ty),
    Value(Vec<Src>),
}

#[derive(Clone, Copy, PartialEq)]
enum ParKind {
    For,
    Reduce,
    On,
}

struct Barrier {
    first_inner: LocalId,
    loop_vars: Vec<LocalId>,
    kind: ParKind,
    /// Outer collections this `par for` writes at its own index (`out[i] = ..`).
    written: HashSet<LocalId>,
    /// Reads of outer collections at some other index: (collection, where).
    cross_reads: Vec<(LocalId, Span)>,
}

impl Barrier {
    fn new(first_inner: LocalId, loop_vars: Vec<LocalId>, kind: ParKind) -> Barrier {
        Barrier { first_inner, loop_vars, kind, written: HashSet::new(), cross_reads: Vec::new() }
    }
}

struct LoopFrame {
    head: BlockId,
    exit: BlockId,
    result: Option<LocalId>,
}

struct FnCtx {
    mir: Mir,
    cur: BlockId,
    exit: BlockId,
    scopes: Vec<Vec<(String, LocalId)>>,
    loops: Vec<LoopFrame>,
    barriers: Vec<Barrier>,
    /// Name resolution stops at an isolated context (a nested `fn` cannot see its parent).
    isolated: bool,
    /// Temporaries that own memory and were borrowed by a call; each dies at the end of its
    /// statement.
    temps: Vec<LocalId>,
}

pub(super) struct Lowerer<'r> {
    prog: &'r Program,
    res: &'r Results,
    pub diags: Vec<Diagnostic>,
    ctxs: Vec<FnCtx>,
    pub done: Vec<Mir>,
    reported: HashSet<(usize, usize)>,
    /// For every closure expression: the variables it captures and how.
    pub captures: HashMap<NodeKey, Vec<(String, CaptureKind)>>,
    /// The next block lowered is a function body (a value leaving it is checked by `Escape`).
    body_block: bool,
}

/// How a closure uses a variable it captures from the enclosing function.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CaptureKind {
    /// Only read: a shared borrow.
    Ref,
    /// Changed (or borrowed `mut`): a mutable borrow.
    MutRef,
    /// Moved out of: the closure takes the value.
    Move,
}

impl<'r> Lowerer<'r> {
    pub fn new(prog: &'r Program, res: &'r Results) -> Lowerer<'r> {
        Lowerer {
            prog,
            res,
            diags: Vec::new(),
            ctxs: Vec::new(),
            done: Vec::new(),
            reported: HashSet::new(),
            captures: HashMap::new(),
            body_block: false,
        }
    }

    // ------------------------------------------------------------ types

    fn ty_of(&self, e: &Expr) -> Ty {
        self.res.expr_ty.get(&NodeKey::of(e)).cloned().unwrap_or(Ty::Error)
    }

    fn pat_ty(&self, p: &Pattern) -> Ty {
        self.res.pat_ty.get(&NodeKey::of(p)).cloned().unwrap_or(Ty::Error)
    }

    fn adt_named(&self, name: &str) -> Option<AdtId> {
        self.prog.find_adt(name)
    }

    fn is_vec(&self, ty: &Ty) -> bool {
        matches!(ty, Ty::Adt(id, _) if Some(*id) == self.adt_named("Vec"))
    }

    fn is_task(&self, ty: &Ty) -> bool {
        matches!(ty, Ty::Adt(id, _) if Some(*id) == self.adt_named("Task"))
    }

    pub(super) fn is_copy(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Int(_) | Ty::Float(_) | Ty::Complex(_) | Ty::Bool | Ty::Char | Ty::Str | Ty::Never => true,
            // views can be duplicated
            Ty::Slice(_) => true,
            Ty::Tuple(ts) => ts.iter().all(|t| self.is_copy(t)),
            Ty::Array(t, _) => self.is_copy(t),
            Ty::Tensor(..) | Ty::Fn(..) => false,
            Ty::Param(g) => self
                .prog
                .generic(*g)
                .bounds
                .iter()
                .any(|b| self.prog.trait_(b.trait_).name == "Copy"),
            Ty::Var(_) | Ty::Error => true,
            Ty::Adt(id, args) => {
                let Some(copy) = self.prog.find_trait("Copy") else { return false };
                for imp in &self.prog.impls {
                    if imp.trait_ref.as_ref().map(|t| t.trait_) != Some(copy) {
                        continue;
                    }
                    let Ty::Adt(iid, params) = &imp.self_ty else { continue };
                    if iid != id {
                        continue;
                    }
                    let ok = params.iter().zip(args).all(|(p, a)| match p {
                        Ty::Param(g) => {
                            let needs_copy = self
                                .prog
                                .generic(*g)
                                .bounds
                                .iter()
                                .any(|b| self.prog.trait_(b.trait_).name == "Copy");
                            !needs_copy || self.is_copy(a)
                        }
                        _ => true,
                    });
                    if ok {
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Can a value of this type hold borrows of other places?
    pub(super) fn carries(&self, ty: &Ty) -> bool {
        self.carries_in(ty, &mut Vec::new())
    }

    fn carries_in(&self, ty: &Ty, seen: &mut Vec<AdtId>) -> bool {
        match ty {
            Ty::Slice(_) | Ty::Str | Ty::Fn(..) => true,
            Ty::Tuple(ts) => ts.iter().any(|t| self.carries_in(t, seen)),
            Ty::Array(t, _) | Ty::Tensor(t, _) => self.carries_in(t, seen),
            Ty::Adt(id, args) => {
                if self.is_task(ty) {
                    return true;
                }
                if args.iter().any(|a| self.carries_in(a, seen)) {
                    return true;
                }
                if seen.contains(id) {
                    return false;
                }
                seen.push(*id);
                let r = match &self.prog.adt(*id).kind {
                    AdtKind::Struct(fs) => fs.iter().any(|f| self.carries_in(&f.ty, seen)),
                    AdtKind::Enum(vs) => vs.iter().any(|v| v.fields.iter().any(|t| self.carries_in(t, seen))),
                };
                seen.pop();
                r
            }
            _ => false,
        }
    }

    /// Can a value of this type keep hold of views it is given (a `Vec<[T]>`, a struct
    /// with a `str` field)? A plain `[f64]` cannot: writing into it copies data.
    pub(super) fn can_store_views(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Slice(t) | Ty::Array(t, _) | Ty::Tensor(t, _) => self.carries(t),
            Ty::Str => false,
            other => self.carries(other),
        }
    }

    // ------------------------------------------------------------ context plumbing

    fn ctx(&mut self) -> &mut FnCtx {
        self.ctxs.last_mut().unwrap()
    }

    fn emit(&mut self, ev: Ev) {
        let c = self.ctx();
        let cur = c.cur;
        c.mir.blocks[cur].events.push(ev);
    }

    fn new_block(&mut self) -> BlockId {
        let c = self.ctx();
        c.mir.blocks.push(BasicBlock { events: Vec::new(), succ: Vec::new() });
        c.mir.blocks.len() - 1
    }

    fn goto(&mut self, to: BlockId) {
        let c = self.ctx();
        let cur = c.cur;
        if !c.mir.blocks[cur].succ.contains(&to) {
            c.mir.blocks[cur].succ.push(to);
        }
    }

    fn switch(&mut self, b: BlockId) {
        self.ctx().cur = b;
    }

    /// Jump away (return/break/continue); following code lands in a dead block.
    fn jump(&mut self, to: BlockId) {
        self.goto(to);
        let dead = self.new_block();
        self.switch(dead);
    }

    fn push_scope(&mut self) {
        self.ctx().scopes.push(Vec::new());
    }

    /// Leave a scope: task handles declared in it are joined here.
    fn pop_scope(&mut self) {
        self.pop_scope_ex(true);
    }

    /// `dead`: also report borrows still in use as the scope's variables are dropped. The scope
    /// of a function body skips it: a value leaving the function is checked by `Escape`.
    fn pop_scope_ex(&mut self, dead: bool) {
        let scope = self.ctx().scopes.pop().unwrap_or_default();
        for (_, id) in scope.iter().rev() {
            if self.ctx().mir.locals[*id].is_task {
                self.emit(Ev::DropUse { local: *id });
            }
        }
        for (_, id) in scope.iter().rev() {
            let info = &self.ctx().mir.locals[*id];
            if dead && info.kind == LocalKind::Owned {
                let span = info.span;
                self.emit(Ev::Dead { local: *id, span });
            }
        }
    }

    /// If `op` is a computed value that owns memory, give it a hidden local so that a call
    /// can borrow it; the local dies at the end of the statement.
    fn materialize(&mut self, op: Operand, e: &Expr) -> Operand {
        let Operand::Value(srcs) = op else { return op };
        let ty = self.ty_of(e);
        // (a closure's own storage cannot be viewed; what it captures is already in `srcs`)
        if self.is_copy(&ty) || matches!(ty, Ty::Error | Ty::Never | Ty::Param(_) | Ty::Fn(..)) {
            return Operand::Value(srcs);
        }
        let id = self.declare("", ty.clone(), LocalKind::Owned, e.span);
        self.emit(Ev::Decl { local: id, init: true });
        if !srcs.is_empty() {
            self.emit(Ev::Hold { holder: id, srcs });
        }
        self.ctx().temps.push(id);
        Operand::Place(Place::local(id), ty)
    }

    /// The statement is over: its temporaries are dropped.
    fn end_statement(&mut self, mark: usize) {
        while self.ctx().temps.len() > mark {
            let id = self.ctx().temps.pop().unwrap();
            let span = self.ctx().mir.locals[id].span;
            self.emit(Ev::Dead { local: id, span });
        }
    }

    fn declare(&mut self, name: &str, ty: Ty, kind: LocalKind, span: Span) -> LocalId {
        let is_task = self.is_task(&ty);
        let c = self.ctx();
        c.mir.locals.push(LocalInfo { name: name.to_string(), ty, kind, span, is_task });
        let id = c.mir.locals.len() - 1;
        if !name.is_empty() {
            c.scopes.last_mut().unwrap().push((name.to_string(), id));
        }
        id
    }

    fn temp(&mut self, ty: Ty, span: Span) -> LocalId {
        let id = self.declare("", ty, LocalKind::Owned, span);
        self.emit(Ev::Decl { local: id, init: true });
        id
    }

    fn local_info(&self, id: LocalId) -> &LocalInfo {
        &self.ctxs.last().unwrap().mir.locals[id]
    }

    /// Resolve a name, capturing it from an enclosing function if needed.
    fn lookup(&mut self, name: &str) -> Option<LocalId> {
        let top = self.ctxs.len() - 1;
        let mut d = top;
        loop {
            if let Some((_, id)) = self.ctxs[d].scopes.iter().rev().flat_map(|s| s.iter().rev()).find(|(n, _)| n == name) {
                let mut id = *id;
                for k in d + 1..=top {
                    id = self.capture_into(k, name, id);
                }
                return Some(id);
            }
            if self.ctxs[d].isolated || d == 0 {
                return None;
            }
            d -= 1;
        }
    }

    fn capture_into(&mut self, k: usize, name: &str, outer: LocalId) -> LocalId {
        if let Some((_, inner)) = self.ctxs[k].mir.captures.iter().find(|(o, _)| *o == outer) {
            return *inner;
        }
        let info = self.ctxs[k - 1].mir.locals[outer].clone();
        let c = &mut self.ctxs[k];
        c.mir.locals.push(LocalInfo {
            name: name.to_string(),
            ty: info.ty,
            kind: LocalKind::Captured,
            span: info.span,
            is_task: false,
        });
        let inner = c.mir.locals.len() - 1;
        c.mir.captures.push((outer, inner));
        inner
    }

    fn place_name(&self, p: &Place) -> String {
        let mir = &self.ctxs.last().unwrap().mir;
        let mut s = mir.locals[p.local].name.clone();
        for pr in &p.proj {
            match pr {
                Proj::Field(f) => {
                    s.push('.');
                    s.push_str(f);
                }
                Proj::Index => s.push_str("[..]"),
            }
        }
        s
    }

    fn error(&mut self, d: Diagnostic) {
        if self.reported.insert((d.span.start, d.span.end)) {
            self.diags.push(d);
        }
    }

    // ------------------------------------------------------------ operands

    fn make_loan(&mut self, place: Place, mutable: bool, span: Span) -> Src {
        let c = self.ctx();
        c.mir.loans.push(Loan { place, mutable, span });
        let id = c.mir.loans.len() - 1;
        self.emit(Ev::Borrow { loan: id });
        Src::Loan(id)
    }

    fn check_movable(&mut self, p: &Place, span: Span) -> bool {
        let info = self.local_info(p.local).clone();
        let name = self.place_name(p);
        if p.has_index() {
            self.error(
                Diagnostic::new(format!("cannot move `{name}` out of a collection"), span)
                    .with_hint("copy the element (the type must be `Copy`), or restructure so the value is built fresh"),
            );
            return false;
        }
        match info.kind {
            LocalKind::Param(ParamMode::Borrow) | LocalKind::Param(ParamMode::Mut) => {
                self.error(
                    Diagnostic::new(format!("cannot move `{name}`: `{}` is only borrowed", info.name), span)
                        .with_note(info.span, "parameter declared here")
                        .with_hint(format!("take ownership with `own {}: T`, or make the type `Copy`", info.name)),
                );
                false
            }
            LocalKind::BorrowBinding => {
                self.error(
                    Diagnostic::new(format!("cannot move `{name}`: it is a borrowed view of another value"), span)
                        .with_note(info.span, "bound here")
                        .with_hint("use it by reference, or copy it if the type is `Copy`"),
                );
                false
            }
            _ => true,
        }
    }

    /// Use a value (copy or move).
    fn consume_value(&mut self, op: Operand, span: Span) -> Vec<Src> {
        match op {
            Operand::Value(s) => s,
            Operand::Place(p, ty) => {
                let carried = if self.carries(&ty) { vec![Src::HeldBy(p.local)] } else { vec![] };
                if self.is_copy(&ty) {
                    self.emit(Ev::Read { place: p, span });
                } else if self.check_movable(&p, span) {
                    self.emit(Ev::Move { place: p, span });
                } else {
                    self.emit(Ev::Read { place: p, span });
                }
                carried
            }
        }
    }

    /// Borrow for a call argument or an operator operand.
    fn consume_borrow(&mut self, op: Operand, mutable: bool, span: Span) -> Vec<Src> {
        match op {
            Operand::Value(s) => s,
            Operand::Place(p, ty) => {
                if mutable {
                    self.check_par_write(&p, span, None);
                    vec![self.make_loan(p, true, span)]
                } else if self.is_copy(&ty) {
                    // a copy of a scalar, or a view that already holds its loans
                    let carried = if self.carries(&ty) { vec![Src::HeldBy(p.local)] } else { vec![] };
                    self.emit(Ev::Read { place: p, span });
                    carried
                } else {
                    vec![self.make_loan(p, false, span)]
                }
            }
        }
    }

    /// Is `indices` just a loop variable of the innermost `par for` (`out[i]`)?
    fn is_loop_index(&self, indices: &[Expr]) -> bool {
        let c = self.ctxs.last().unwrap();
        let Some(b) = c.barriers.last() else { return false };
        indices.len() == 1
            && matches!(&indices[0].kind, ExprKind::Ident(n)
                if c.scopes.iter().rev().flat_map(|s| s.iter().rev())
                    .find(|(m, _)| m == n)
                    .map_or(false, |(_, id)| b.loop_vars.contains(id)))
    }

    fn check_par_write(&mut self, p: &Place, span: Span, lhs: Option<&Expr>) {
        let Some(b) = self.ctxs.last().unwrap().barriers.last() else { return };
        if p.local >= b.first_inner {
            return;
        }
        let exempt = b.kind == ParKind::For
            && p.proj == [Proj::Index]
            && matches!(lhs.map(|e| &e.kind), Some(ExprKind::Index { indices, .. }) if self.is_loop_index(indices));
        if exempt {
            self.ctx().barriers.last_mut().unwrap().written.insert(p.local);
            return;
        }
        let b = self.ctxs.last().unwrap().barriers.last().unwrap();
        let name = self.place_name(p);
        let (what, hint) = match b.kind {
            ParKind::For => (
                "a `par for`: its iterations run at the same time",
                "write `out[i]` with the loop index, use `par_chunks`, or accumulate with `par sum`",
            ),
            ParKind::Reduce => ("a parallel reduction", "a reduction body must only read; it returns its result"),
            ParKind::On => ("an `on` block, which runs on other nodes", "return the result from the block instead"),
        };
        self.error(Diagnostic::new(format!("cannot modify `{name}` inside {what}"), span).with_hint(hint));
    }

    // ------------------------------------------------------------ statements and blocks

    fn lower_block(&mut self, b: &Block) -> Operand {
        let is_body = std::mem::take(&mut self.body_block);
        self.push_scope();
        let mut result = Operand::Value(vec![]);
        let n = b.stmts.len();
        for (i, s) in b.stmts.iter().enumerate() {
            let mark = self.ctx().temps.len();
            match s {
                Stmt::Let { pat, init, span, .. } => {
                    self.lower_let(pat, init.as_ref(), *span);
                    self.end_statement(mark);
                }
                Stmt::Expr(e) => {
                    let op = self.lower(e);
                    if i + 1 == n {
                        // the value of the block: its temporaries live until the enclosing
                        // statement ends
                        result = op;
                    } else {
                        if let Operand::Place(p, _) = op {
                            self.emit(Ev::Read { place: p, span: e.span });
                        }
                        self.end_statement(mark);
                    }
                }
                Stmt::Item(item) => {
                    if let ItemKind::Fn(f) = &item.kind {
                        self.lower_fn_decl(f, true);
                    }
                }
            }
        }
        // a value leaving the block must not borrow from the block's own variables: park it in
        // a holder that lives in the enclosing scope, so the check at their end sees it
        if let Operand::Value(srcs) = &result {
            if !is_body && !srcs.is_empty() && self.ctx().scopes.len() >= 2 {
                let srcs = srcs.clone();
                let scope = self.ctx().scopes.pop().unwrap_or_default();
                let holder = self.declare("", Ty::Error, LocalKind::Owned, b.span);
                self.emit(Ev::Decl { local: holder, init: true });
                self.emit(Ev::Hold { holder, srcs });
                self.ctx().scopes.push(scope);
                result = Operand::Value(vec![Src::HeldBy(holder)]);
            }
        }
        self.pop_scope_ex(!is_body);
        result
    }

    /// The simple binding patterns in `p` (the first alternative of `|` patterns).
    fn pattern_bindings<'p>(&self, p: &'p Pattern, out: &mut Vec<&'p Pattern>) {
        match &p.kind {
            PatKind::Ident { name, mutable } => {
                if !*mutable && self.is_unit_variant(p, name) {
                    return;
                }
                out.push(p);
            }
            PatKind::Tuple(ps) => ps.iter().for_each(|x| self.pattern_bindings(x, out)),
            PatKind::Variant { args, .. } => args.iter().for_each(|x| self.pattern_bindings(x, out)),
            PatKind::Or(alts) => self.pattern_bindings(&alts[0], out),
            _ => {}
        }
    }

    fn is_unit_variant(&self, p: &Pattern, name: &str) -> bool {
        match self.pat_ty(p) {
            Ty::Adt(id, _) => match &self.prog.adt(id).kind {
                AdtKind::Enum(vs) => vs.iter().any(|v| v.name == name && v.fields.is_empty()),
                _ => false,
            },
            _ => false,
        }
    }

    /// Declare the bindings of a pattern. Each holds `srcs` if its type can carry borrows.
    fn bind_pattern(&mut self, pat: &Pattern, kind: LocalKind, srcs: &[Src], has_init: bool) -> Vec<LocalId> {
        let mut binds = Vec::new();
        self.pattern_bindings(pat, &mut binds);
        let mut ids = Vec::new();
        for b in binds {
            let PatKind::Ident { name, .. } = &b.kind else { continue };
            let ty = self.pat_ty(b);
            let k = if self.is_copy(&ty) && !self.carries(&ty) { LocalKind::Owned } else { kind };
            let id = self.declare(name, ty.clone(), k, b.span);
            self.emit(Ev::Decl { local: id, init: has_init });
            // A view binding aliases its source even when its type could not carry a
            // borrow by itself (a `String` payload, say), so it always holds the loan.
            let aliases = k == LocalKind::BorrowBinding;
            if has_init && (self.carries(&ty) || aliases) && !srcs.is_empty() {
                self.emit(Ev::Hold { holder: id, srcs: srcs.to_vec() });
            }
            ids.push(id);
        }
        ids
    }

    fn lower_let(&mut self, pat: &Pattern, init: Option<&Expr>, span: Span) {
        match init {
            Some(e) => {
                let op = self.lower(e);
                let srcs = self.consume_value(op, e.span);
                self.bind_pattern(pat, LocalKind::Owned, &srcs, true);
            }
            None => {
                self.bind_pattern(pat, LocalKind::Owned, &[], false);
            }
        }
        let _ = span;
    }

    // ------------------------------------------------------------ expressions

    fn lower_value(&mut self, e: &Expr) -> Vec<Src> {
        let op = self.lower(e);
        self.consume_value(op, e.span)
    }

    fn lower(&mut self, e: &Expr) -> Operand {
        match &e.kind {
            ExprKind::Int { .. }
            | ExprKind::Float { .. }
            | ExprKind::Imag(_)
            | ExprKind::Bool(_)
            | ExprKind::Char(_)
            | ExprKind::TypeApp { .. } => Operand::Value(vec![]),
            ExprKind::Str(pieces) => {
                for p in pieces {
                    if let StrPiece::Interp(inner, _) = p {
                        let op = self.lower(inner);
                        self.consume_borrow(op, false, inner.span);
                    }
                }
                Operand::Value(vec![])
            }
            ExprKind::Ident(name) => match self.lookup(name) {
                Some(id) => Operand::Place(Place::local(id), self.ty_of(e)),
                None => Operand::Value(vec![]),
            },
            ExprKind::Tuple(es) | ExprKind::Array(es) => {
                let mut srcs = Vec::new();
                for x in es {
                    srcs.extend(self.lower_value(x));
                }
                self.aggregate(e, srcs)
            }
            ExprKind::ArrayRepeat { value, len } => {
                let srcs = self.lower_value(value);
                self.lower_value(len);
                self.aggregate(e, srcs)
            }
            ExprKind::Unary { op: _, expr } => {
                if let Some(Res::Fn(fid, _)) = self.res.expr_res.get(&NodeKey::of(e)) {
                    let sig = self.prog.fn_(*fid).sig.clone();
                    let operand = self.lower(expr);
                    let mode = sig.self_mode.unwrap_or(ParamMode::Borrow);
                    return self.call_with_modes(e, vec![(operand, mode)], &[expr.span]);
                }
                let op = self.lower(expr);
                self.consume_borrow(op, false, expr.span);
                Operand::Value(vec![])
            }
            ExprKind::Binary { lhs, rhs, .. } => self.lower_binary(e, lhs, rhs),
            ExprKind::Cast { expr, .. } => {
                let op = self.lower(expr);
                self.consume_borrow(op, false, expr.span);
                Operand::Value(vec![])
            }
            ExprKind::Assign { op, lhs, rhs } => {
                self.lower_assign(e, op.is_some(), lhs, rhs);
                Operand::Value(vec![])
            }
            ExprKind::Call { callee, args } => self.lower_call(e, callee, args),
            ExprKind::MethodCall { recv, args, .. } => self.lower_method_call(e, recv, args),
            ExprKind::Field { base, name } => match self.lower(base) {
                Operand::Place(p, _) => Operand::Place(p.field(&name.name), self.ty_of(e)),
                v => v,
            },
            ExprKind::Index { base, indices } => self.lower_index(e, base, indices),
            ExprKind::Try(inner) => {
                let srcs = self.lower_value(inner);
                // `?` may return early
                let cont = self.new_block();
                let exit = self.ctx().exit;
                self.goto(cont);
                self.goto(exit);
                self.switch(cont);
                Operand::Value(if self.carries(&self.ty_of(e)) { srcs } else { vec![] })
            }
            ExprKind::Range { start, end, .. } => {
                for x in [start, end].into_iter().flatten() {
                    self.lower_value(x);
                }
                Operand::Value(vec![])
            }
            ExprKind::Closure { params, body, .. } => self.lower_closure(e, params, ClosureBody::Expr(body), false),
            ExprKind::Block(b) | ExprKind::Unsafe(b) => self.lower_block(b),
            ExprKind::If { cond, then, else_ } => self.lower_if(e, cond, then, else_.as_deref()),
            ExprKind::Match { scrutinee, arms } => self.lower_match(e, scrutinee, arms),
            ExprKind::Loop(b) => self.lower_loop(e, b),
            ExprKind::While { cond, body } => {
                let head = self.new_block();
                let body_b = self.new_block();
                let exit = self.new_block();
                self.goto(head);
                self.switch(head);
                self.lower_value(cond);
                self.goto(body_b);
                self.goto(exit);
                self.ctx().loops.push(LoopFrame { head, exit, result: None });
                self.switch(body_b);
                self.lower_block(body);
                self.goto(head);
                self.ctx().loops.pop();
                self.switch(exit);
                Operand::Value(vec![])
            }
            ExprKind::For { par, pat, iter, guard, body } => {
                self.lower_for(*par, pat, iter, guard.as_deref(), body);
                Operand::Value(vec![])
            }
            ExprKind::Return(v) => {
                let ret_ty = match v {
                    Some(x) => self.ty_of(x),
                    None => Ty::unit(),
                };
                if let Some(x) = v {
                    let srcs = self.lower_value(x);
                    if self.carries(&ret_ty) {
                        self.emit(Ev::Escape { srcs, span: x.span });
                    }
                }
                let exit = self.ctx().exit;
                self.jump(exit);
                Operand::Value(vec![])
            }
            ExprKind::Break(v) => {
                let frame = self.ctxs.last().unwrap().loops.last().map(|f| (f.exit, f.result));
                if let Some(x) = v {
                    let srcs = self.lower_value(x);
                    if let Some((_, Some(t))) = frame {
                        if self.carries(&self.local_info(t).ty.clone()) {
                            self.emit(Ev::Hold { holder: t, srcs });
                        }
                    }
                }
                if let Some((exit, _)) = frame {
                    self.jump(exit);
                }
                Operand::Value(vec![])
            }
            ExprKind::Continue => {
                if let Some(head) = self.ctxs.last().unwrap().loops.last().map(|f| f.head) {
                    self.jump(head);
                }
                Operand::Value(vec![])
            }
            ExprKind::MutBorrow(inner) => self.lower(inner),
            // `own x` moves `x` out (a copy for a `Copy` type); the result is a value, not a place
            ExprKind::Own(inner) => {
                let op = self.lower(inner);
                Operand::Value(self.consume_value(op, inner.span))
            }
            ExprKind::ParReduce { pat, iter, body, .. } => {
                self.lower_par_reduce(pat, iter, body);
                Operand::Value(vec![])
            }
            ExprKind::Spawn(b) => self.lower_closure(e, &[], ClosureBody::Block(b), true),
            ExprKind::On { target, body } => {
                self.lower_value(target);
                self.push_scope();
                let first = self.ctx().mir.locals.len();
                self.ctx().barriers.push(Barrier::new(first, vec![], ParKind::On));
                let op = self.lower_block(body);
                let srcs = self.consume_value(op, body.span);
                self.ctx().barriers.pop();
                self.pop_scope();
                Operand::Value(if self.carries(&self.ty_of(e)) { srcs } else { vec![] })
            }
        }
    }

    /// A tuple/array/struct value built from `srcs`.
    fn aggregate(&mut self, e: &Expr, srcs: Vec<Src>) -> Operand {
        Operand::Value(if self.carries(&self.ty_of(e)) { srcs } else { vec![] })
    }

    // ------------------------------------------------------------ calls

    /// Lower arguments that were already evaluated to operands, create the loans for
    /// them just before the call, and return the call's result.
    fn call_with_modes(&mut self, e: &Expr, args: Vec<(Operand, ParamMode)>, spans: &[Span]) -> Operand {
        let mut groups = Vec::new();
        // `mut` arguments that could keep hold of the views other arguments carry
        let mut stores: Vec<(usize, LocalId)> = Vec::new();
        for (i, (op, mode)) in args.into_iter().enumerate() {
            let span = spans.get(i).copied().unwrap_or(e.span);
            if let (ParamMode::Mut, Operand::Place(p, ty)) = (mode, &op) {
                if self.can_store_views(ty) {
                    stores.push((i, p.local));
                }
            }
            let g = match mode {
                ParamMode::Borrow => self.consume_borrow(op, false, span),
                ParamMode::Mut => self.consume_borrow(op, true, span),
                ParamMode::Own => self.consume_value(op, span),
            };
            groups.push(g);
        }
        let all: Vec<Src> = groups.iter().flatten().cloned().collect();
        if groups.iter().any(|g| !g.is_empty()) {
            self.emit(Ev::Call { groups: groups.clone(), span: e.span });
        }
        for (i, local) in stores {
            let flowing: Vec<Src> = groups
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .flat_map(|(_, g)| g.iter().cloned())
                .collect();
            if !flowing.is_empty() {
                self.emit(Ev::HoldAdd { holder: local, srcs: flowing });
            }
        }
        Operand::Value(if self.carries(&self.ty_of(e)) { all } else { vec![] })
    }

    /// An argument expression: `mut x` is a mutable borrow marker.
    fn lower_arg(&mut self, a: &Expr) -> Operand {
        let inner = match &a.kind {
            ExprKind::MutBorrow(inner) => inner,
            _ => a,
        };
        let op = self.lower(inner);
        self.materialize(op, inner)
    }

    fn lower_call(&mut self, e: &Expr, callee: &Expr, args: &[Expr]) -> Operand {
        let res = self.res.expr_res.get(&NodeKey::of(callee)).cloned();
        let modes: Vec<ParamMode> = match &res {
            Some(Res::Fn(fid, _)) => self.prog.fn_(*fid).sig.params.iter().map(|p| p.mode).collect(),
            // constructors take their fields by value
            Some(Res::StructCtor(..)) | Some(Res::Variant(..)) => vec![ParamMode::Own; args.len()],
            _ => vec![ParamMode::Borrow; args.len()],
        };
        let mut ops = Vec::new();
        let mut spans = Vec::new();
        // calling a value of function type uses the function value
        if res.is_none() || matches!(res, Some(Res::Local(_))) {
            let op = self.lower(callee);
            if let Operand::Place(p, _) = op {
                self.emit(Ev::Read { place: p, span: callee.span });
            }
        }
        for (i, a) in args.iter().enumerate() {
            let mode = modes.get(i).copied().unwrap_or(ParamMode::Borrow);
            ops.push((self.lower_arg(a), mode));
            spans.push(a.span);
        }
        let out = self.call_with_modes(e, ops, &spans);
        // a constructor's result carries the borrows of its fields
        out
    }

    fn lower_method_call(&mut self, e: &Expr, recv: &Expr, args: &[Expr]) -> Operand {
        let res = self.res.expr_res.get(&NodeKey::of(e)).cloned();
        let (self_mode, modes): (Option<ParamMode>, Vec<ParamMode>) = match &res {
            Some(Res::Fn(fid, _)) => {
                let sig = &self.prog.fn_(*fid).sig;
                (sig.self_mode, sig.params.iter().map(|p| p.mode).collect())
            }
            Some(Res::StructCtor(..)) | Some(Res::Variant(..)) => (None, vec![ParamMode::Own; args.len()]),
            // `t.sum()`: the tensor is only read
            Some(Res::Builtin(k)) if crate::check::tensor_builtin_has_receiver(k) => (Some(ParamMode::Borrow), vec![ParamMode::Borrow; args.len()]),
            _ => (None, vec![ParamMode::Borrow; args.len()]),
        };
        let mut ops = Vec::new();
        let mut spans = Vec::new();
        if let Some(m) = self_mode {
            // `Type.f(x)` has no receiver value; `v.f(x)` does
            let r = self.lower(recv);
            ops.push((self.materialize(r, recv), m));
            spans.push(recv.span);
        }
        for (i, a) in args.iter().enumerate() {
            let mode = modes.get(i).copied().unwrap_or(ParamMode::Borrow);
            ops.push((self.lower_arg(a), mode));
            spans.push(a.span);
        }
        self.call_with_modes(e, ops, &spans)
    }

    fn lower_binary(&mut self, e: &Expr, lhs: &Expr, rhs: &Expr) -> Operand {
        if let Some(Res::Fn(fid, _)) = self.res.expr_res.get(&NodeKey::of(e)) {
            let sig = self.prog.fn_(*fid).sig.clone();
            let lm = sig.self_mode.unwrap_or(ParamMode::Borrow);
            let rm = sig.params.first().map(|p| p.mode).unwrap_or(ParamMode::Borrow);
            let l = self.lower(lhs);
            let r = self.lower(rhs);
            return self.call_with_modes(e, vec![(l, lm), (r, rm)], &[lhs.span, rhs.span]);
        }
        let l = self.lower(lhs);
        let r = self.lower(rhs);
        self.consume_borrow(l, false, lhs.span);
        self.consume_borrow(r, false, rhs.span);
        Operand::Value(vec![])
    }

    fn lower_index(&mut self, e: &Expr, base: &Expr, indices: &[Expr]) -> Operand {
        // `vec[1, 2, 3]` builds a vector
        if matches!(&base.kind, ExprKind::Ident(n) if n == "vec") && self.lookup("vec").is_none() {
            let mut srcs = Vec::new();
            for i in indices {
                srcs.extend(self.lower_value(i));
            }
            return self.aggregate(e, srcs);
        }
        let b = self.lower(base);
        let is_range = indices.len() == 1 && matches!(indices[0].kind, ExprKind::Range { .. });
        for i in indices {
            let op = self.lower(i);
            self.consume_value(op, i.span);
        }
        // part of a tensor is a copy of it: the tensor is read, and nothing keeps borrowing it
        if self.is_tensor_slice(base, indices) {
            self.consume_borrow(b, false, e.span);
            return Operand::Value(vec![]);
        }
        // inside a `par for`: reading an outer collection at some index other than the
        // loop variable may race with another iteration writing that element
        if let Operand::Place(p, _) = &b {
            let c = self.ctxs.last().unwrap();
            if let Some(bar) = c.barriers.last() {
                if bar.kind == ParKind::For && p.local < bar.first_inner && !self.is_loop_index(indices) {
                    let local = p.local;
                    self.ctx().barriers.last_mut().unwrap().cross_reads.push((local, e.span));
                }
            }
        }
        match b {
            Operand::Place(p, _) if is_range => {
                // a slice view of the whole collection
                Operand::Value(vec![self.make_loan(p, false, e.span)])
            }
            Operand::Place(p, _) => Operand::Place(p.index(), self.ty_of(e)),
            v => v,
        }
    }

    // ------------------------------------------------------------ assignment

    fn lower_assign(&mut self, e: &Expr, compound: bool, lhs: &Expr, rhs: &Expr) {
        let rhs_op = self.lower(rhs);
        if compound {
            // `a += b`: an overloaded operator borrows both; a built-in one reads them
            let l = self.lower(lhs);
            let place = match &l {
                Operand::Place(p, _) => Some(p.clone()),
                _ => None,
            };
            let srcs = if let Some(Res::Fn(fid, _)) = self.res.expr_res.get(&NodeKey::of(e)) {
                let sig = self.prog.fn_(*fid).sig.clone();
                let lm = sig.self_mode.unwrap_or(ParamMode::Borrow);
                let rm = sig.params.first().map(|p| p.mode).unwrap_or(ParamMode::Borrow);
                let r = self.call_with_modes(e, vec![(l, lm), (rhs_op, rm)], &[lhs.span, rhs.span]);
                match r {
                    Operand::Value(s) => s,
                    _ => vec![],
                }
            } else {
                self.consume_borrow(l, false, lhs.span);
                self.consume_borrow(rhs_op, false, rhs.span);
                vec![]
            };
            if let Some(p) = place {
                self.write(p, srcs, lhs.span, Some(lhs));
            }
            return;
        }
        // `t[0..2, 1] = x` writes the elements of `x` into the tensor `t`: `x` is only read
        if let ExprKind::Index { base, indices } = &lhs.kind {
            if self.is_tensor_slice(base, indices) {
                self.consume_borrow(rhs_op, false, rhs.span);
                let b = self.lower(base);
                for i in indices {
                    let op = self.lower(i);
                    self.consume_value(op, i.span);
                }
                if let Operand::Place(p, _) = b {
                    self.write(p.index(), vec![], lhs.span, Some(lhs));
                }
                return;
            }
        }
        let srcs = self.consume_value(rhs_op, rhs.span);
        let l = self.lower(lhs);
        if let Operand::Place(p, _) = l {
            self.write(p, srcs, lhs.span, Some(lhs));
        }
    }

    /// `t[0..2, 1]` on a tensor `t`: an index with a range, which makes a copy.
    fn is_tensor_slice(&self, base: &Expr, indices: &[Expr]) -> bool {
        matches!(self.ty_of(base), Ty::Tensor(..)) && indices.iter().any(|i| matches!(i.kind, ExprKind::Range { .. }))
    }

    fn write(&mut self, p: Place, srcs: Vec<Src>, span: Span, lhs: Option<&Expr>) {
        self.check_par_write(&p, span, lhs);
        let whole = p.proj.is_empty();
        let local = p.local;
        self.emit(Ev::Write { place: p, span });
        if self.carries(&self.local_info(local).ty.clone()) {
            if whole {
                self.emit(Ev::Hold { holder: local, srcs });
            } else {
                self.emit(Ev::HoldAdd { holder: local, srcs });
            }
        }
    }

    // ------------------------------------------------------------ control flow

    /// Combine branch results into one value held by a temporary.
    fn join_result(&mut self, e: &Expr, join: BlockId, tmp: Option<LocalId>) -> Operand {
        self.switch(join);
        let _ = e;
        match tmp {
            Some(t) => Operand::Value(vec![Src::HeldBy(t)]),
            None => Operand::Value(vec![]),
        }
    }

    fn branch_value(&mut self, op: Operand, span: Span, tmp: Option<LocalId>) {
        let srcs = self.consume_value(op, span);
        if let Some(t) = tmp {
            self.emit(Ev::Hold { holder: t, srcs });
        }
    }

    fn result_temp(&mut self, e: &Expr) -> Option<LocalId> {
        let ty = self.ty_of(e);
        if self.carries(&ty) {
            Some(self.temp(ty, e.span))
        } else {
            None
        }
    }

    fn lower_if(&mut self, e: &Expr, cond: &Expr, then: &Block, else_: Option<&Expr>) -> Operand {
        self.lower_value(cond);
        let tmp = self.result_temp(e);
        let then_b = self.new_block();
        let else_b = self.new_block();
        let join = self.new_block();
        self.goto(then_b);
        self.goto(else_b);
        self.switch(then_b);
        let t = self.lower_block(then);
        let unit_if = else_.is_none();
        if unit_if {
            if let Operand::Place(p, _) = t {
                self.emit(Ev::Read { place: p, span: then.span });
            }
        } else {
            self.branch_value(t, then.span, tmp);
        }
        self.goto(join);
        self.switch(else_b);
        if let Some(el) = else_ {
            let op = self.lower(el);
            self.branch_value(op, el.span, tmp);
        }
        self.goto(join);
        self.join_result(e, join, tmp)
    }

    fn lower_match(&mut self, e: &Expr, scrutinee: &Expr, arms: &[MatchArm]) -> Operand {
        let scrut = self.lower(scrutinee);
        // Does any arm bind a non-copy part of the scrutinee? Then the bindings are views.
        let mut binds_noncopy = false;
        for a in arms {
            let mut bs = Vec::new();
            self.pattern_bindings(&a.pat, &mut bs);
            for b in bs {
                if !self.is_copy(&self.pat_ty(b)) {
                    binds_noncopy = true;
                }
            }
        }
        let (kind, srcs): (LocalKind, Vec<Src>) = match scrut {
            Operand::Place(p, _) if binds_noncopy => {
                (LocalKind::BorrowBinding, vec![self.make_loan(p, false, scrutinee.span)])
            }
            Operand::Place(p, _) => {
                self.emit(Ev::Read { place: p, span: scrutinee.span });
                (LocalKind::Owned, vec![])
            }
            Operand::Value(s) => (LocalKind::Owned, s),
        };
        // keep the scrutinee's loans alive while the arms run
        let hold = if !srcs.is_empty() {
            let t = self.temp(self.ty_of(scrutinee), scrutinee.span);
            self.emit(Ev::Hold { holder: t, srcs: srcs.clone() });
            Some(t)
        } else {
            None
        };
        let tmp = self.result_temp(e);
        let join = self.new_block();
        let start = self.ctx().cur;
        for a in arms {
            let arm_b = self.new_block();
            self.switch(start);
            self.goto(arm_b);
            self.switch(arm_b);
            self.push_scope();
            self.bind_pattern(&a.pat, kind, &srcs, true);
            if let Some(g) = &a.guard {
                self.lower_value(g);
            }
            if let Some(t) = hold {
                self.emit(Ev::Touch { local: t });
            }
            let op = self.lower(&a.body);
            self.branch_value(op, a.body.span, tmp);
            self.pop_scope();
            self.goto(join);
        }
        if arms.is_empty() {
            self.switch(start);
            self.goto(join);
        }
        self.join_result(e, join, tmp)
    }

    fn lower_loop(&mut self, e: &Expr, body: &Block) -> Operand {
        let tmp = self.result_temp(e);
        let head = self.new_block();
        let exit = self.new_block();
        self.goto(head);
        self.switch(head);
        self.ctx().loops.push(LoopFrame { head, exit, result: tmp });
        self.lower_block(body);
        self.goto(head);
        self.ctx().loops.pop();
        self.join_result(e, exit, tmp)
    }

    fn lower_for(&mut self, par: bool, pat: &Pattern, iter: &Expr, guard: Option<&Expr>, body: &Block) {
        let (inner, mut_iter) = match &iter.kind {
            ExprKind::MutBorrow(i) => (&**i, true),
            _ => (iter, false),
        };
        let iter_ty = self.ty_of(inner);
        let op = self.lower(inner);
        let is_collection = matches!(iter_ty, Ty::Array(..) | Ty::Slice(_))
            || matches!(&iter_ty, Ty::Adt(..) if self.is_vec(&iter_ty));
        // an iterator is used up by the loop (`for x in mut it` only borrows it); the items it gives are owned
        let is_iterator = self.res.for_next.contains_key(&NodeKey::of(iter));
        let (srcs, borrowed) = match op {
            Operand::Place(p, _) if mut_iter => (vec![self.make_loan(p, true, iter.span)], !is_iterator),
            Operand::Place(p, ty) if !is_iterator && !self.is_copy(&ty) => (vec![self.make_loan(p, false, iter.span)], true),
            op => (self.consume_value(op, iter.span), is_collection),
        };
        let it = self.temp(iter_ty, iter.span);
        if !srcs.is_empty() {
            self.emit(Ev::Hold { holder: it, srcs });
        }
        let head = self.new_block();
        let body_b = self.new_block();
        let exit = self.new_block();
        self.goto(head);
        self.switch(head);
        self.emit(Ev::Touch { local: it });
        self.goto(body_b);
        self.goto(exit);
        self.ctx().loops.push(LoopFrame { head, exit, result: None });
        self.switch(body_b);
        self.push_scope();
        let first = self.ctx().mir.locals.len();
        let kind = if borrowed { LocalKind::BorrowBinding } else { LocalKind::Owned };
        let ids = self.bind_pattern(pat, kind, &[Src::HeldBy(it)], true);
        if par {
            self.ctx().barriers.push(Barrier::new(first, ids.clone(), ParKind::For));
        }
        if let Some(g) = guard {
            self.lower_value(g);
        }
        self.lower_block(body);
        if par {
            let b = self.ctx().barriers.pop().unwrap();
            for (local, span) in &b.cross_reads {
                if b.written.contains(local) {
                    let name = self.local_info(*local).name.clone();
                    self.error(
                        Diagnostic::new(
                            format!("cannot read `{name}[..]` here: other iterations of this `par for` write `{name}` at the same time"),
                            *span,
                        )
                        .with_hint(format!("read only `{name}[i]` with the loop index, or read from a separate input slice")),
                    );
                }
            }
        }
        self.pop_scope();
        self.goto(head);
        self.ctx().loops.pop();
        self.switch(exit);
    }

    fn lower_par_reduce(&mut self, pat: &Pattern, iter: &Expr, body: &Expr) {
        let iter_ty = self.ty_of(iter);
        let op = self.lower(iter);
        let (srcs, borrowed) = match op {
            Operand::Place(p, ty) if !self.is_copy(&ty) => (vec![self.make_loan(p, false, iter.span)], true),
            op => (self.consume_value(op, iter.span), false),
        };
        let it = self.temp(iter_ty, iter.span);
        if !srcs.is_empty() {
            self.emit(Ev::Hold { holder: it, srcs });
        }
        let head = self.new_block();
        let body_b = self.new_block();
        let exit = self.new_block();
        self.goto(head);
        self.switch(head);
        self.emit(Ev::Touch { local: it });
        self.goto(body_b);
        self.goto(exit);
        self.switch(body_b);
        self.push_scope();
        let first = self.ctx().mir.locals.len();
        let kind = if borrowed { LocalKind::BorrowBinding } else { LocalKind::Owned };
        self.bind_pattern(pat, kind, &[Src::HeldBy(it)], true);
        self.ctx().barriers.push(Barrier::new(first, vec![], ParKind::Reduce));
        let op = self.lower(body);
        self.consume_value(op, body.span);
        self.ctx().barriers.pop();
        self.pop_scope();
        self.goto(head);
        self.switch(exit);
    }

    // ------------------------------------------------------------ closures and tasks

    fn lower_closure(&mut self, e: &Expr, params: &[ClosureParam], body: ClosureBody, is_spawn: bool) -> Operand {
        let mut mir = Mir::new(if is_spawn { "spawn" } else { "closure" });
        mir.blocks.push(BasicBlock { events: Vec::new(), succ: Vec::new() }); // exit block
        self.ctxs.push(FnCtx {
            mir,
            cur: 0,
            exit: 1,
            scopes: vec![Vec::new()],
            loops: Vec::new(),
            barriers: Vec::new(),
            isolated: false,
            temps: Vec::new(),
        });
        for p in params {
            let mut bs = Vec::new();
            self.pattern_bindings(&p.pat, &mut bs);
            for b in bs {
                if let PatKind::Ident { name, .. } = &b.kind {
                    let ty = self.pat_ty(b);
                    let id = self.declare(name, ty, LocalKind::Param(ParamMode::Borrow), b.span);
                    self.emit(Ev::Decl { local: id, init: true });
                }
            }
        }
        let op = match body {
            ClosureBody::Expr(x) => self.lower(x),
            ClosureBody::Block(b) => {
                self.body_block = true;
                self.lower_block(b)
            }
        };
        let span = e.span;
        let ret_ty = match body {
            ClosureBody::Expr(x) => self.ty_of(x),
            ClosureBody::Block(_) => Ty::Error,
        };
        if !ret_ty.is_unit() || matches!(body, ClosureBody::Block(_)) {
            self.consume_value(op, span);
        }
        let exit = self.ctx().exit;
        self.goto(exit);
        let ctx = self.ctxs.pop().unwrap();
        let mir = ctx.mir;

        // how does the body use each captured variable?
        let mut srcs = Vec::new();
        let captures = mir.captures.clone();
        let mut table = Vec::new();
        for (outer, inner) in &captures {
            let kind = classify_capture(&mir, *inner);
            let ty = self.local_info(*outer).ty.clone();
            let name = self.local_info(*outer).name.clone();
            table.push((name.clone(), kind));
            if is_spawn && kind == CaptureKind::MutRef {
                let at = first_mutation(&mir, *inner).unwrap_or(span);
                self.error(
                    Diagnostic::new(format!("cannot modify `{name}` from inside `spawn`"), at)
                        .with_hint("a task runs at the same time as the code around it; return the result from the task, or move the value in"),
                );
            }
            let place = Place::local(*outer);
            match kind {
                CaptureKind::Move if !self.is_copy(&ty) => {
                    srcs.extend(self.consume_value(Operand::Place(place, ty), span));
                }
                CaptureKind::MutRef => srcs.push(self.make_loan(place, true, span)),
                _ => srcs.push(self.make_loan(place, false, span)),
            }
        }
        self.captures.insert(NodeKey::of(e), table);
        self.done.push(mir);
        Operand::Value(if self.carries(&self.ty_of(e)) { srcs } else { vec![] })
    }

    // ------------------------------------------------------------ functions

    pub fn lower_fn_decl(&mut self, f: &FnDecl, nested: bool) {
        let Some(body) = &f.body else { return };
        let Some(&fid) = self.res.fn_decl.get(&NodeKey::of(f)) else { return };
        let def = self.prog.fn_(fid).clone();
        let mut mir = Mir::new(&f.name.name);
        mir.blocks.push(BasicBlock { events: Vec::new(), succ: Vec::new() });
        let _ = nested;
        self.ctxs.push(FnCtx {
            mir,
            cur: 0,
            exit: 1,
            scopes: vec![Vec::new()],
            loops: Vec::new(),
            barriers: Vec::new(),
            isolated: true,
            temps: Vec::new(),
        });
        if let Some(mode) = def.sig.self_mode {
            let self_ty = match def.parent {
                Parent::Impl(i) => self.prog.impl_(i).self_ty.clone(),
                Parent::Trait(t) => Ty::Param(self.prog.trait_(t).self_generic),
                Parent::Free => Ty::Error,
            };
            let span = f.params.first().map(|p| p.span).unwrap_or(f.name.span);
            let id = self.declare("self", self_ty, LocalKind::Param(mode), span);
            self.emit(Ev::Decl { local: id, init: true });
        }
        for (p, pd) in f.params.iter().filter(|p| p.name.name != "self").zip(&def.sig.params) {
            let id = self.declare(&pd.name, pd.ty.clone(), LocalKind::Param(pd.mode), p.span);
            self.emit(Ev::Decl { local: id, init: true });
        }
        self.body_block = true;
        let op = self.lower_block(body);
        if !def.sig.ret.is_unit() {
            let span = match body.stmts.last() {
                Some(Stmt::Expr(e)) => e.span,
                _ => body.span,
            };
            let srcs = self.consume_value(op, span);
            if self.carries(&def.sig.ret) {
                self.emit(Ev::Escape { srcs, span });
            }
        }
        let exit = self.ctx().exit;
        self.goto(exit);
        let ctx = self.ctxs.pop().unwrap();
        self.done.push(ctx.mir);
    }
}

#[derive(Clone, Copy)]
enum ClosureBody<'a> {
    Expr(&'a Expr),
    Block(&'a Block),
}

fn classify_capture(mir: &Mir, inner: LocalId) -> CaptureKind {
    let mut kind = CaptureKind::Ref;
    for b in &mir.blocks {
        for ev in &b.events {
            match ev {
                Ev::Move { place, .. } if place.local == inner => return CaptureKind::Move,
                Ev::Write { place, .. } if place.local == inner => kind = CaptureKind::MutRef,
                Ev::Borrow { loan } if mir.loans[*loan].place.local == inner && mir.loans[*loan].mutable => {
                    kind = CaptureKind::MutRef
                }
                _ => {}
            }
        }
    }
    kind
}

fn first_mutation(mir: &Mir, inner: LocalId) -> Option<Span> {
    for b in &mir.blocks {
        for ev in &b.events {
            match ev {
                Ev::Write { place, span } if place.local == inner => return Some(*span),
                Ev::Borrow { loan } if mir.loans[*loan].place.local == inner && mir.loans[*loan].mutable => {
                    return Some(mir.loans[*loan].span)
                }
                _ => {}
            }
        }
    }
    None
}
