//! A small control-flow graph of *events*, lowered from the typed AST.
//!
//! The borrow checker does not care about arithmetic or types; it cares about which
//! places are read, moved, written and borrowed, in which order, along which control
//! flow paths. Everything else is dropped during lowering.

use crate::ast::ParamMode;
use crate::span::Span;
use crate::types::Ty;

pub type LocalId = usize;
pub type LoanId = usize;
pub type BlockId = usize;

#[derive(Clone, Debug, PartialEq)]
pub enum Proj {
    Field(String),
    /// Any element: `v[i]` (all indices are treated as the same place).
    Index,
}

/// A variable and a path into it: `p.pos.x` is `Place { local: p, proj: [pos, x] }`.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub local: LocalId,
    pub proj: Vec<Proj>,
}

impl Place {
    pub fn local(l: LocalId) -> Place {
        Place { local: l, proj: Vec::new() }
    }

    pub fn field(&self, name: &str) -> Place {
        let mut p = self.clone();
        p.proj.push(Proj::Field(name.to_string()));
        p
    }

    pub fn index(&self) -> Place {
        let mut p = self.clone();
        p.proj.push(Proj::Index);
        p
    }

    pub fn is_prefix_of(&self, other: &Place) -> bool {
        self.local == other.local && other.proj.starts_with(&self.proj)
    }

    /// Two places overlap when one is a prefix of the other: `p` and `p.x` overlap,
    /// `p.x` and `p.y` do not.
    pub fn overlaps(&self, other: &Place) -> bool {
        self.is_prefix_of(other) || other.is_prefix_of(self)
    }

    pub fn has_index(&self) -> bool {
        self.proj.iter().any(|p| matches!(p, Proj::Index))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LocalKind {
    /// A parameter. `Borrow`/`Mut` parameters are borrowed from the caller: their
    /// contents cannot be moved out. `Own` parameters are owned.
    Param(ParamMode),
    /// A variable that owns its value.
    Owned,
    /// A binding that views part of another place (a `for` element, a pattern
    /// binding of a non-copy payload): it cannot be moved out of.
    BorrowBinding,
    /// A variable captured from an enclosing function by a closure or task.
    Captured,
}

#[derive(Clone, Debug)]
pub struct LocalInfo {
    pub name: String,
    pub ty: Ty,
    pub kind: LocalKind,
    pub span: Span,
    /// Is this a task handle? Dropping one joins the task, so loans it holds last
    /// until it goes out of scope.
    pub is_task: bool,
}

#[derive(Clone, Debug)]
pub struct Loan {
    pub place: Place,
    pub mutable: bool,
    pub span: Span,
}

/// Where the borrows carried by a value come from.
#[derive(Clone, Debug, PartialEq)]
pub enum Src {
    Loan(LoanId),
    /// Whatever the local currently holds (the value was copied or moved from it).
    HeldBy(LocalId),
}

#[derive(Clone, Debug, PartialEq)]
pub enum MoveKind {
    Moved,
    /// Declared but not yet assigned.
    Uninit,
}

#[derive(Clone, Debug)]
pub enum Ev {
    /// Copy or inspect a place.
    Read { place: Place, span: Span },
    /// Take a non-copy value out of a place.
    Move { place: Place, span: Span },
    /// Assign to a place (whole or partial). A whole-place write re-initialises it.
    Write { place: Place, span: Span },
    /// Create a loan (checks against active loans).
    Borrow { loan: LoanId },
    /// `holder` now holds exactly these borrows.
    Hold { holder: LocalId, srcs: Vec<Src> },
    /// `holder` additionally holds these borrows (partial assignment).
    HoldAdd { holder: LocalId, srcs: Vec<Src> },
    /// The borrows passed to one call, one group per argument: groups must not conflict.
    Call { groups: Vec<Vec<Src>>, span: Span },
    /// A local comes into scope. Without an initial value it starts uninitialised.
    Decl { local: LocalId, init: bool },
    /// Scope exit for a task handle: keeps it (and its loans) alive until here.
    DropUse { local: LocalId },
    /// Keep a local alive at this point (loop iterators).
    Touch { local: LocalId },
    /// A value leaves the function: its borrows must not point at locals.
    Escape { srcs: Vec<Src>, span: Span },
    /// A local (a variable at the end of its scope, or a temporary at the end of its
    /// statement) is dropped here: nothing may still be using a borrow of it.
    Dead { local: LocalId, span: Span },
}

#[derive(Clone, Debug)]
pub struct BasicBlock {
    pub events: Vec<Ev>,
    pub succ: Vec<BlockId>,
}

#[derive(Clone, Debug)]
pub struct Mir {
    pub name: String,
    pub blocks: Vec<BasicBlock>,
    pub locals: Vec<LocalInfo>,
    pub loans: Vec<Loan>,
    pub entry: BlockId,
    /// Locals captured from the enclosing function, in order: (outer, inner).
    pub captures: Vec<(LocalId, LocalId)>,
}

impl Mir {
    pub fn new(name: &str) -> Mir {
        Mir {
            name: name.to_string(),
            blocks: vec![BasicBlock { events: Vec::new(), succ: Vec::new() }],
            locals: Vec::new(),
            loans: Vec::new(),
            entry: 0,
            captures: Vec::new(),
        }
    }
}
