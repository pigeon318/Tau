//! Type inference variables and unification.
//!
//! There are three kinds of variable. A *general* variable can become any type. An
//! *integer-literal* variable (the type of `5`) can become any integer, float or
//! complex type; a *float-literal* variable (`5.0`) any float or complex type. This
//! lets `2 * x` work for `x: f64` while variables stay strictly typed. Unconstrained
//! literal variables default to `i64` and `f64`.

use crate::span::Span;
use crate::types::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VarKind {
    General,
    Int,
    Float,
}

#[derive(Clone, Debug)]
pub struct VarState {
    pub kind: VarKind,
    pub binding: Option<Ty>,
    /// Where the variable was created (for "cannot infer" errors).
    pub span: Span,
}

#[derive(Clone, Default, Debug)]
pub struct Infer {
    pub vars: Vec<VarState>,
}

impl Infer {
    pub fn fresh(&mut self, kind: VarKind, span: Span) -> Ty {
        self.vars.push(VarState {
            kind,
            binding: None,
            span,
        });
        Ty::Var(TyVar(self.vars.len() as u32 - 1))
    }

    pub fn kind_of(&self, v: TyVar) -> VarKind {
        self.vars[v.idx()].kind
    }

    /// Follow variable bindings at the top level only.
    pub fn shallow(&self, t: &Ty) -> Ty {
        let mut cur = t.clone();
        while let Ty::Var(v) = &cur {
            match &self.vars[v.idx()].binding {
                Some(b) => cur = b.clone(),
                None => break,
            }
        }
        cur
    }

    /// Resolve all bound variables inside the type.
    pub fn resolve(&self, t: &Ty) -> Ty {
        let t = self.shallow(t);
        t.map_children(&|x| match x {
            Ty::Var(_) => {
                let s = self.shallow(x);
                if matches!(s, Ty::Var(_)) {
                    Some(s)
                } else {
                    Some(self.resolve(&s))
                }
            }
            _ => None,
        })
    }

    /// Like `resolve`, but any variable still unbound becomes `Ty::Error`. Used when
    /// writing results out, so no variable outlives the table it belongs to.
    pub fn zonk(&self, t: &Ty) -> Ty {
        self.resolve(t).map_children(&|x| match x {
            Ty::Var(_) => Some(Ty::Error),
            _ => None,
        })
    }

    pub fn snapshot(&self) -> Vec<VarState> {
        self.vars.clone()
    }

    pub fn rollback(&mut self, snap: Vec<VarState>) {
        self.vars = snap;
    }

    /// Unify two types, returning false on mismatch. May leave partial bindings on
    /// failure, so callers that need to probe should use `snapshot`/`rollback`.
    pub fn unify(&mut self, a: &Ty, b: &Ty) -> bool {
        let a = self.shallow(a);
        let b = self.shallow(b);
        match (&a, &b) {
            // An error type poisons a variable instead of leaving it unresolved, so one
            // mistake does not also produce a "cannot infer" error.
            (Ty::Error, Ty::Var(v)) | (Ty::Var(v), Ty::Error) => {
                self.vars[v.idx()].binding = Some(Ty::Error);
                true
            }
            (Ty::Error, _) | (_, Ty::Error) => true,
            (Ty::Var(x), Ty::Var(y)) => {
                if x == y {
                    return true;
                }
                let kx = self.vars[x.idx()].kind;
                let ky = self.vars[y.idx()].kind;
                let k = match (kx, ky) {
                    (VarKind::General, k) | (k, VarKind::General) => k,
                    (VarKind::Int, VarKind::Int) => VarKind::Int,
                    _ => VarKind::Float, // int-literal meets float-literal
                };
                self.vars[y.idx()].kind = k;
                self.vars[x.idx()].binding = Some(Ty::Var(*y));
                true
            }
            (Ty::Var(v), t) | (t, Ty::Var(v)) => self.bind(*v, t),
            (Ty::Int(x), Ty::Int(y)) => x == y,
            (Ty::Float(x), Ty::Float(y)) => x == y,
            (Ty::Complex(x), Ty::Complex(y)) => x == y,
            (Ty::Bool, Ty::Bool) | (Ty::Char, Ty::Char) | (Ty::Str, Ty::Str) => true,
            (Ty::Never, Ty::Never) => true,
            (Ty::Tuple(x), Ty::Tuple(y)) => self.unify_all(x, y),
            (Ty::Array(x, nx), Ty::Array(y, ny)) => {
                if let (Some(p), Some(q)) = (nx, ny) {
                    if p != q {
                        return false;
                    }
                }
                self.unify(x, y)
            }
            (Ty::Slice(x), Ty::Slice(y)) => self.unify(x, y),
            (Ty::Tensor(x, dx), Ty::Tensor(y, dy)) => self.unify(x, y) && dims_compatible(dx, dy),
            (Ty::Adt(i, x), Ty::Adt(j, y)) => i == j && self.unify_all(x, y),
            (Ty::Fn(px, rx), Ty::Fn(py, ry)) => self.unify_all(px, py) && self.unify(rx, ry),
            (Ty::Param(x), Ty::Param(y)) => x == y,
            _ => false,
        }
    }

    fn unify_all(&mut self, xs: &[Ty], ys: &[Ty]) -> bool {
        xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| self.unify(x, y))
    }

    fn bind(&mut self, v: TyVar, t: &Ty) -> bool {
        match self.vars[v.idx()].kind {
            VarKind::General => {
                if self.resolve(t).contains_var(v) {
                    return false; // occurs check
                }
            }
            VarKind::Int => {
                if !matches!(t, Ty::Int(_) | Ty::Float(_) | Ty::Complex(_)) {
                    return false;
                }
            }
            VarKind::Float => {
                if !matches!(t, Ty::Float(_) | Ty::Complex(_)) {
                    return false;
                }
            }
        }
        self.vars[v.idx()].binding = Some(t.clone());
        true
    }

    /// Give unconstrained literal variables their default type. Returns true if
    /// any variable was defaulted.
    pub fn apply_defaults(&mut self) {
        for i in 0..self.vars.len() {
            if self.vars[i].binding.is_none() {
                match self.vars[i].kind {
                    VarKind::Int => self.vars[i].binding = Some(Ty::Int(IntTy::I64)),
                    VarKind::Float => self.vars[i].binding = Some(Ty::Float(FloatTy::F64)),
                    VarKind::General => {}
                }
            }
        }
    }
}

/// Shapes are compatible when ranks agree and every pair of dimensions could be
/// equal. Symbolic and dynamic dimensions are checked at run time in v1.
pub fn dims_compatible(a: &[Dim], b: &[Dim]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| match (x, y) {
            (Dim::Fixed(p), Dim::Fixed(q)) => p == q,
            _ => true,
        })
}
