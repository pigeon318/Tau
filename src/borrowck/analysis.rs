//! Dataflow over the event CFG, and the checks.
//!
//! Two analyses run on each body:
//!  * *Backward liveness* of locals. A loan is only alive while a local that holds it is
//!    still going to be used (non-lexical lifetimes).
//!  * *Forward state*: which places may have been moved, and which loans each local holds.
//!
//! Each event is then checked against the state just before it and the liveness just
//! after it.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::mir::*;
use crate::ast::ParamMode;
use crate::diag::Diagnostic;
use crate::span::Span;

#[derive(Clone, Default)]
struct State {
    moved: Vec<(Place, Span, MoveKind)>,
    holds: BTreeMap<LocalId, BTreeSet<LoanId>>,
    /// Loans a task handle held when it was consumed (so they can still be passed on).
    moved_holds: BTreeMap<LocalId, BTreeSet<LoanId>>,
}

impl State {
    /// Union `other` into `self`; returns whether anything changed.
    fn merge(&mut self, other: &State) -> bool {
        let mut changed = false;
        for m in &other.moved {
            if !self.moved.iter().any(|x| x.0 == m.0 && x.2 == m.2) {
                self.moved.push(m.clone());
                changed = true;
            }
        }
        for (l, set) in &other.holds {
            let mine = self.holds.entry(*l).or_default();
            for loan in set {
                changed |= mine.insert(*loan);
            }
        }
        for (l, set) in &other.moved_holds {
            let mine = self.moved_holds.entry(*l).or_default();
            for loan in set {
                changed |= mine.insert(*loan);
            }
        }
        changed
    }
}

fn uses_defs(mir: &Mir, ev: &Ev) -> (Vec<LocalId>, Vec<LocalId>) {
    let held = |srcs: &[Src]| -> Vec<LocalId> {
        srcs.iter()
            .filter_map(|s| if let Src::HeldBy(l) = s { Some(*l) } else { None })
            .collect()
    };
    match ev {
        Ev::Read { place, .. } | Ev::Move { place, .. } => (vec![place.local], vec![]),
        Ev::Write { place, .. } => {
            if place.proj.is_empty() {
                (vec![], vec![place.local])
            } else {
                (vec![place.local], vec![])
            }
        }
        Ev::Borrow { loan } => (vec![mir.loans[*loan].place.local], vec![]),
        Ev::Hold { holder, srcs } => (held(srcs), vec![*holder]),
        Ev::HoldAdd { holder, srcs } => {
            let mut u = held(srcs);
            u.push(*holder);
            (u, vec![])
        }
        Ev::Call { groups, .. } => (groups.iter().flat_map(|g| held(g)).collect(), vec![]),
        Ev::Decl { local, .. } => (vec![], vec![*local]),
        Ev::DropUse { local } | Ev::Touch { local } => (vec![*local], vec![]),
        Ev::Escape { srcs, .. } => (held(srcs), vec![]),
    }
}

struct Cx<'m> {
    mir: &'m Mir,
    diags: Vec<Diagnostic>,
    seen: HashSet<(usize, usize, &'static str)>,
    /// One error per move site, however many later uses there are.
    reported_moves: HashSet<(usize, usize)>,
}

impl<'m> Cx<'m> {
    fn name(&self, p: &Place) -> String {
        let mut s = self.mir.locals[p.local].name.clone();
        if s.is_empty() {
            s = "<temporary>".to_string();
        }
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

    fn error(&mut self, kind: &'static str, d: Diagnostic) {
        if self.seen.insert((d.span.start, d.span.end, kind)) {
            self.diags.push(d);
        }
    }

    fn resolve(&self, st: &State, srcs: &[Src]) -> BTreeSet<LoanId> {
        let mut out = BTreeSet::new();
        for s in srcs {
            match s {
                Src::Loan(l) => {
                    out.insert(*l);
                }
                Src::HeldBy(h) => {
                    if let Some(set) = st.holds.get(h).or_else(|| st.moved_holds.get(h)) {
                        out.extend(set.iter().copied());
                    }
                }
            }
        }
        out
    }

    /// Loans held by a local that is still going to be used.
    fn active(&self, st: &State, live_after: &BTreeSet<LocalId>) -> Vec<(LoanId, LocalId)> {
        let mut out = Vec::new();
        for (h, loans) in &st.holds {
            if live_after.contains(h) {
                for l in loans {
                    out.push((*l, *h));
                }
            }
        }
        out
    }

    fn check_moved(&mut self, st: &State, place: &Place, span: Span) {
        let hit = st.moved.iter().find(|(m, _, _)| m.overlaps(place)).cloned();
        let Some((m, at, kind)) = hit else { return };
        if !self.reported_moves.insert((at.start, at.end)) {
            return;
        }
        let d = match kind {
            MoveKind::Uninit => {
                let name = self.name(place);
                Diagnostic::new(format!("use of possibly-uninitialized `{name}`"), span)
                    .with_hint("assign a value on every path before using it")
            }
            MoveKind::Moved => {
                let moved_name = self.name(&m);
                let title = if m.is_prefix_of(place) {
                    format!("use of moved value `{moved_name}`")
                } else {
                    format!("use of partially moved value `{}`", self.name(place))
                };
                Diagnostic::new(title, span)
                    .with_note(at, format!("`{moved_name}` moved here"))
                    .with_hint("a value is moved when it is assigned, returned, stored, or passed to an `own` parameter; borrow it instead, or give its type an `impl Copy`")
            }
        };
        self.error("moved", d);
    }

    fn conflict_note(&self, loan: &Loan, holder: Option<LocalId>) -> (Span, String) {
        let how = if loan.mutable { "mutably borrowed here" } else { "borrowed here" };
        let msg = match holder {
            Some(h) if !self.mir.locals[h].name.is_empty() => {
                format!("{how}, and still used later through `{}`", self.mir.locals[h].name)
            }
            _ => format!("{how}, and still in use"),
        };
        (loan.span, msg)
    }

    fn step(&mut self, st: &mut State, ev: &Ev, live_after: &BTreeSet<LocalId>, report: bool) {
        match ev {
            Ev::Read { place, span } => {
                if report {
                    self.check_moved(st, place, *span);
                    for (l, h) in self.active(st, live_after) {
                        let loan = &self.mir.loans[l];
                        if loan.mutable && loan.place.overlaps(place) {
                            let name = self.name(&loan.place);
                            let (ns, nm) = self.conflict_note(loan, Some(h));
                            self.error(
                                "read",
                                Diagnostic::new(format!("cannot use `{name}` because it is mutably borrowed"), *span).with_note(ns, nm),
                            );
                        }
                    }
                }
            }
            Ev::Move { place, span } => {
                if report {
                    // a live borrow is the more useful thing to report, so check it first
                    for (l, h) in self.active(st, live_after) {
                        let loan = &self.mir.loans[l];
                        if loan.place.overlaps(place) {
                            let name = self.name(place);
                            let (ns, nm) = self.conflict_note(loan, Some(h));
                            self.error(
                                "move",
                                Diagnostic::new(format!("cannot move out of `{name}` because it is borrowed"), *span).with_note(ns, nm),
                            );
                        }
                    }
                    self.check_moved(st, place, *span);
                }
                st.moved.push((place.clone(), *span, MoveKind::Moved));
                // joining a task consumes the handle, which ends the borrows it held
                if place.proj.is_empty() && self.mir.locals[place.local].is_task {
                    if let Some(h) = st.holds.remove(&place.local) {
                        st.moved_holds.insert(place.local, h);
                    }
                }
            }
            Ev::Write { place, span } => {
                if report {
                    // assigning to part of a moved value
                    let partial_of_moved = st.moved.iter().find(|(m, _, k)| {
                        *k == MoveKind::Moved && m.is_prefix_of(place) && m.proj.len() < place.proj.len()
                    });
                    if let Some((m, at, _)) = partial_of_moved.cloned() {
                        let name = self.name(&m);
                        self.error(
                            "partial",
                            Diagnostic::new(format!("cannot assign to part of moved value `{name}`"), *span)
                                .with_note(at, "value moved here"),
                        );
                    }
                    for (l, h) in self.active(st, live_after) {
                        let loan = &self.mir.loans[l];
                        if loan.place.overlaps(place) {
                            let name = self.name(place);
                            let (ns, nm) = self.conflict_note(loan, Some(h));
                            self.error(
                                "write",
                                Diagnostic::new(format!("cannot assign to `{name}` because it is borrowed"), *span).with_note(ns, nm),
                            );
                        }
                    }
                }
                // a write re-initialises the place and everything inside it
                st.moved.retain(|(m, _, _)| !place.is_prefix_of(m));
            }
            Ev::Borrow { loan } => {
                let ln = &self.mir.loans[*loan];
                if report {
                    self.check_moved(st, &ln.place, ln.span);
                    for (l, h) in self.active(st, live_after) {
                        let other = &self.mir.loans[l];
                        if !other.place.overlaps(&ln.place) {
                            continue;
                        }
                        let name = self.name(&ln.place);
                        let (ns, nm) = self.conflict_note(other, Some(h));
                        if ln.mutable {
                            self.error(
                                "bmut",
                                Diagnostic::new(format!("cannot borrow `{name}` as mutable because it is also borrowed"), ln.span)
                                    .with_note(ns, nm),
                            );
                        } else if other.mutable {
                            self.error(
                                "bshared",
                                Diagnostic::new(format!("cannot borrow `{name}` because it is mutably borrowed"), ln.span)
                                    .with_note(ns, nm),
                            );
                        }
                    }
                }
            }
            Ev::Hold { holder, srcs } => {
                let loans = self.resolve(st, srcs);
                st.holds.insert(*holder, loans);
            }
            Ev::HoldAdd { holder, srcs } => {
                let loans = self.resolve(st, srcs);
                st.holds.entry(*holder).or_default().extend(loans);
            }
            Ev::Call { groups, span } => {
                if report {
                    let resolved: Vec<BTreeSet<LoanId>> = groups.iter().map(|g| self.resolve(st, g)).collect();
                    for i in 0..resolved.len() {
                        for j in i + 1..resolved.len() {
                            for &a in &resolved[i] {
                                for &b in &resolved[j] {
                                    if a == b {
                                        continue;
                                    }
                                    let (la, lb) = (&self.mir.loans[a], &self.mir.loans[b]);
                                    if (la.mutable || lb.mutable) && la.place.overlaps(&lb.place) {
                                        let (m, o) = if la.mutable { (la, lb) } else { (lb, la) };
                                        let name = self.name(&m.place);
                                        self.error(
                                            "call",
                                            Diagnostic::new(
                                                format!("cannot borrow `{name}` as mutable while it is also used by another argument of this call"),
                                                *span,
                                            )
                                            .with_note(m.span, "mutable borrow here")
                                            .with_note(o.span, "also used here")
                                            .with_hint("a value that is borrowed `mut` cannot be passed anywhere else in the same call"),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ev::Decl { local, init } => {
                let l = *local;
                st.moved.retain(|(m, _, _)| m.local != l);
                st.holds.remove(&l);
                if !init {
                    st.moved.push((Place::local(l), self.mir.locals[l].span, MoveKind::Uninit));
                }
            }
            Ev::DropUse { .. } | Ev::Touch { .. } => {}
            Ev::Escape { srcs, span } => {
                if report {
                    for l in self.resolve(st, srcs) {
                        let loan = &self.mir.loans[l];
                        let root = &self.mir.locals[loan.place.local];
                        let borrowed_from_caller = matches!(
                            root.kind,
                            LocalKind::Param(ParamMode::Borrow) | LocalKind::Param(ParamMode::Mut) | LocalKind::BorrowBinding
                        );
                        if !borrowed_from_caller {
                            let name = self.name(&loan.place);
                            self.error(
                                "escape",
                                Diagnostic::new(format!("cannot return a value that borrows `{name}`, which is local to this function"), *span)
                                    .with_note(loan.span, "borrowed here")
                                    .with_hint("a function can only return borrows of its borrowed parameters; return an owned value instead"),
                            );
                        }
                    }
                }
            }
        }
    }
}

pub fn analyze(mir: &Mir) -> Vec<Diagnostic> {
    let n = mir.blocks.len();
    // reachable blocks
    let mut reach = vec![false; n];
    let mut stack = vec![mir.entry];
    while let Some(b) = stack.pop() {
        if reach[b] {
            continue;
        }
        reach[b] = true;
        stack.extend(mir.blocks[b].succ.iter().copied());
    }

    // ---- liveness (backward)
    let mut live_in: Vec<BTreeSet<LocalId>> = vec![BTreeSet::new(); n];
    loop {
        let mut changed = false;
        for b in (0..n).rev() {
            if !reach[b] {
                continue;
            }
            let mut cur: BTreeSet<LocalId> = BTreeSet::new();
            for s in &mir.blocks[b].succ {
                cur.extend(live_in[*s].iter().copied());
            }
            for ev in mir.blocks[b].events.iter().rev() {
                let (u, d) = uses_defs(mir, ev);
                for x in d {
                    cur.remove(&x);
                }
                cur.extend(u);
            }
            if cur != live_in[b] {
                live_in[b] = cur;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // live-after sets for every event
    let mut live_after: Vec<Vec<BTreeSet<LocalId>>> = vec![Vec::new(); n];
    for b in 0..n {
        if !reach[b] {
            continue;
        }
        let mut cur: BTreeSet<LocalId> = BTreeSet::new();
        for s in &mir.blocks[b].succ {
            cur.extend(live_in[*s].iter().copied());
        }
        let evs = &mir.blocks[b].events;
        let mut sets = vec![BTreeSet::new(); evs.len()];
        for (i, ev) in evs.iter().enumerate().rev() {
            sets[i] = cur.clone();
            let (u, d) = uses_defs(mir, ev);
            for x in d {
                cur.remove(&x);
            }
            cur.extend(u);
        }
        live_after[b] = sets;
    }

    // ---- forward state
    let mut cx = Cx { mir, diags: Vec::new(), seen: HashSet::new(), reported_moves: HashSet::new() };
    let mut in_state: Vec<Option<State>> = vec![None; n];
    in_state[mir.entry] = Some(State::default());
    let mut work: Vec<BlockId> = vec![mir.entry];
    let mut guard = 0;
    while let Some(b) = work.pop() {
        guard += 1;
        if guard > 100_000 {
            break;
        }
        let mut st = in_state[b].clone().unwrap();
        for (i, ev) in mir.blocks[b].events.iter().enumerate() {
            cx.step(&mut st, ev, &live_after[b][i], false);
        }
        for &s in &mir.blocks[b].succ {
            match &mut in_state[s] {
                None => {
                    in_state[s] = Some(st.clone());
                    work.push(s);
                }
                Some(existing) => {
                    if existing.merge(&st) {
                        work.push(s);
                    }
                }
            }
        }
    }

    // ---- report
    for b in 0..n {
        let Some(st0) = &in_state[b] else { continue };
        let mut st = st0.clone();
        for (i, ev) in mir.blocks[b].events.iter().enumerate() {
            cx.step(&mut st, ev, &live_after[b][i], true);
        }
    }
    cx.diags
}
