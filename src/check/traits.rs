//! Trait solving, method lookup and signature instantiation.

use super::*;
use crate::infer::VarKind;

/// A method (or associated function) chosen by lookup.
pub(crate) struct Picked {
    pub fn_id: FnId,
    /// Substitution for the generics of the enclosing impl/trait.
    pub subst: Vec<(GenericId, Ty)>,
    /// How many `Box` layers of the receiver were looked through to find it.
    pub boxes: u32,
}

/// A function signature with generics replaced by inference variables.
pub(crate) struct Inst {
    pub self_mode: Option<ParamMode>,
    pub params: Vec<(String, ParamMode, Ty)>,
    pub ret: Ty,
    pub subst: Vec<(GenericId, Ty)>,
}

impl<'a> Checker<'a> {
    pub(crate) fn fresh_subst(&mut self, gens: &[GenericId], span: Span) -> Vec<(GenericId, Ty)> {
        gens.iter().map(|g| (*g, self.fresh(span))).collect()
    }

    fn string_adt(&self) -> Option<AdtId> {
        self.prog.find_adt("String")
    }

    fn vec_adt(&self) -> Option<AdtId> {
        self.prog.find_adt("Vec")
    }

    pub(crate) fn is_vec(&self, t: &Ty) -> Option<Ty> {
        match t {
            Ty::Adt(id, args) if Some(*id) == self.vec_adt() && args.len() == 1 => Some(args[0].clone()),
            _ => None,
        }
    }

    pub(crate) fn is_string(&self, t: &Ty) -> bool {
        matches!(t, Ty::Adt(id, _) if Some(*id) == self.string_adt())
    }

    // ------------------------------------------------------------ obligations

    pub(crate) fn add_bound_obligations(
        &mut self,
        gens: &[GenericId],
        subst: &[(GenericId, Ty)],
        span: Span,
    ) {
        for g in gens {
            let Some((_, ty)) = subst.iter().find(|(k, _)| k == g) else { continue };
            let ty = ty.clone();
            for b in self.prog.generic(*g).bounds.clone() {
                self.body.obligations.push(Obligation {
                    ty: ty.clone(),
                    trait_: b.trait_,
                    args: b.args.iter().map(|a| a.subst(subst)).collect(),
                    span,
                });
            }
        }
    }

    // ------------------------------------------------------------ implements

    pub(crate) fn implements(&mut self, ty: &Ty, tr: TraitId, args: &[Ty]) -> bool {
        self.implements_depth(ty, tr, args, 0)
    }

    fn implements_depth(&mut self, ty: &Ty, tr: TraitId, args: &[Ty], depth: u32) -> bool {
        if depth > 6 {
            return false;
        }
        let ty = self.infer.resolve(ty);
        let tdef = self.prog.trait_(tr).clone();
        match &ty {
            Ty::Error => return true,
            Ty::Var(v) => {
                return match self.infer.kind_of(*v) {
                    VarKind::General => true, // unknown yet; checked again if it resolves
                    VarKind::Int => self.implements_depth(&Ty::Int(IntTy::I64), tr, args, depth + 1),
                    VarKind::Float => {
                        self.implements_depth(&Ty::Float(FloatTy::F64), tr, args, depth + 1)
                    }
                };
            }
            Ty::Param(g) => {
                return self
                    .prog
                    .generic(*g)
                    .bounds
                    .iter()
                    .any(|b| b.trait_ == tr);
            }
            _ => {}
        }
        if tdef.builtin_rule && self.builtin_satisfies(&tdef.name, &ty, depth) {
            return true;
        }
        for i in 0..self.prog.impls.len() {
            let imp = self.prog.impls[i].clone();
            let Some(tref) = &imp.trait_ref else { continue };
            if tref.trait_ != tr {
                continue;
            }
            let snap = self.infer.snapshot();
            let subst = self.fresh_subst(&imp.generics, Span::default());
            let mut ok = self.infer.unify(&imp.self_ty.subst(&subst), &ty);
            if ok && !args.is_empty() && tref.args.len() == args.len() {
                for (a, b) in tref.args.iter().zip(args) {
                    ok = ok && self.infer.unify(&a.subst(&subst), b);
                }
            }
            ok = ok && self.generic_bounds_hold(&imp.generics, &subst, depth);
            self.infer.rollback(snap);
            if ok {
                return true;
            }
        }
        false
    }

    fn generic_bounds_hold(&mut self, gens: &[GenericId], subst: &[(GenericId, Ty)], depth: u32) -> bool {
        for g in gens {
            let Some((_, t)) = subst.iter().find(|(k, _)| k == g) else { continue };
            let t = t.clone();
            for b in self.prog.generic(*g).bounds.clone() {
                let bargs: Vec<Ty> = b.args.iter().map(|a| a.subst(subst)).collect();
                if !self.implements_depth(&t, b.trait_, &bargs, depth + 1) {
                    return false;
                }
            }
        }
        true
    }

    fn builtin_satisfies(&mut self, name: &str, ty: &Ty, depth: u32) -> bool {
        let all = |this: &mut Self, ts: &[Ty], tn: &str| -> bool {
            let Some(tr) = this.prog.find_trait(tn) else { return false };
            ts.iter().all(|t| this.implements_depth(t, tr, &[], depth + 1))
        };
        let is_scalar = matches!(ty, Ty::Int(_) | Ty::Float(_) | Ty::Complex(_) | Ty::Bool | Ty::Char);
        match name {
            "Num" => ty.is_numeric(),
            "Int" => matches!(ty, Ty::Int(_)),
            "Float" => matches!(ty, Ty::Float(_)),
            "Clone" => true,
            "Copy" => match ty {
                Ty::Tuple(ts) => all(self, ts, "Copy"),
                Ty::Array(t, _) => all(self, std::slice::from_ref(t), "Copy"),
                Ty::Never | Ty::Str | Ty::Slice(_) => true,
                _ => is_scalar,
            },
            "Eq" | "Ord" => match ty {
                Ty::Complex(_) if name == "Ord" => false,
                Ty::Str => true,
                Ty::Tuple(ts) => all(self, ts, name),
                Ty::Array(t, _) | Ty::Slice(t) => all(self, std::slice::from_ref(t), name),
                Ty::Adt(..) => {
                    if self.is_string(ty) {
                        true
                    } else if let Some(e) = self.is_vec(ty) {
                        all(self, &[e], name)
                    } else if let Some(e) = self.box_inner(ty) {
                        all(self, &[e], name)
                    } else {
                        false
                    }
                }
                _ => is_scalar,
            },
            "Display" => match ty {
                Ty::Str => true,
                Ty::Tuple(ts) => all(self, ts, "Display"),
                Ty::Array(t, _) | Ty::Slice(t) | Ty::Tensor(t, _) => {
                    all(self, std::slice::from_ref(t), "Display")
                }
                Ty::Adt(..) => {
                    if self.is_string(ty) {
                        true
                    } else if let Some(e) = self.is_vec(ty) {
                        all(self, &[e], "Display")
                    } else if let Some(e) = self.box_inner(ty) {
                        all(self, &[e], "Display")
                    } else {
                        false
                    }
                }
                _ => is_scalar,
            },
            _ => false,
        }
    }

    // ------------------------------------------------------------ method lookup

    /// The receiver types to try, in order: the type itself, then what it
    /// coerces to (arrays and vectors view as slices, `String` as `str`).
    pub(crate) fn deref_chain(&self, ty: &Ty) -> Vec<(Ty, u32)> {
        let mut out = vec![(ty.clone(), 0)];
        let mut cur = ty.clone();
        let mut boxes = 0;
        // a box is looked through to what it holds
        while let Some(inner) = self.box_inner(&cur) {
            boxes += 1;
            out.push((inner.clone(), boxes));
            cur = inner;
        }
        match &cur {
            Ty::Array(t, _) => out.push((Ty::Slice(t.clone()), boxes)),
            Ty::Adt(..) => {
                if let Some(e) = self.is_vec(&cur) {
                    out.push((Ty::Slice(Box::new(e)), boxes));
                } else if self.is_string(&cur) {
                    out.push((Ty::Str, boxes));
                }
            }
            _ => {}
        }
        out
    }

    /// `T` if `ty` is `Box<T>`.
    pub(crate) fn box_inner(&self, ty: &Ty) -> Option<Ty> {
        match ty {
            Ty::Adt(id, args) if args.len() == 1 && self.prog.adt(*id).name == "Box" && self.prog.adt(*id).module == 0 => Some(args[0].clone()),
            _ => None,
        }
    }

    /// Find a method named `name` on `recv`. With `want_self` the method must take
    /// `self` (`value.method()`); otherwise it must not (`Type.function()`).
    pub(crate) fn lookup_method(&mut self, recv: &Ty, name: &str, want_self: bool, span: Span) -> Option<Picked> {
        let recv = self.infer.shallow(recv);
        for (cand, boxes) in self.deref_chain(&recv) {
            let cand = self.infer.shallow(&cand);
            if let Ty::Param(g) = &cand {
                for b in self.prog.generic(*g).bounds.clone() {
                    let tdef = self.prog.trait_(b.trait_).clone();
                    if let Some((_, fid)) = tdef.methods.iter().find(|(n, _)| n == name) {
                        if self.prog.fn_(*fid).sig.self_mode.is_some() != want_self {
                            continue;
                        }
                        let mut subst = vec![(tdef.self_generic, cand.clone())];
                        for (tg, a) in tdef.generics.iter().zip(&b.args) {
                            subst.push((*tg, a.clone()));
                        }
                        return Some(Picked { fn_id: *fid, subst, boxes });
                    }
                }
                continue;
            }
            // inherent impls first, then trait impls
            for want_trait in [false, true] {
                for i in 0..self.prog.impls.len() {
                    let imp = self.prog.impls[i].clone();
                    if imp.trait_ref.is_some() != want_trait {
                        continue;
                    }
                    let (fid, is_default) = match imp.methods.iter().find(|(n, _)| n == name) {
                        Some((_, f)) => (*f, false),
                        None => {
                            // a default method provided by the trait
                            let Some(tref) = &imp.trait_ref else { continue };
                            let tdef = self.prog.trait_(tref.trait_);
                            match tdef.methods.iter().find(|(n, _)| n == name) {
                                Some((_, f)) if self.prog.fn_(*f).has_body => (*f, true),
                                _ => continue,
                            }
                        }
                    };
                    if self.prog.fn_(fid).sig.self_mode.is_some() != want_self {
                        continue;
                    }
                    let snap = self.infer.snapshot();
                    let mut subst = self.fresh_subst(&imp.generics, span);
                    let ok = self.infer.unify(&imp.self_ty.subst(&subst), &cand)
                        && self.generic_bounds_hold(&imp.generics, &subst, 0);
                    if ok {
                        if is_default {
                            let tref = imp.trait_ref.clone().unwrap();
                            let tdef = self.prog.trait_(tref.trait_).clone();
                            subst.push((tdef.self_generic, cand.clone()));
                            for (tg, a) in tdef.generics.iter().zip(&tref.args) {
                                subst.push((*tg, a.subst(&subst)));
                            }
                        }
                        return Some(Picked { fn_id: fid, subst, boxes });
                    }
                    self.infer.rollback(snap);
                }
            }
        }
        None
    }

    /// Find the impl of an operator trait for `lhs` whose method accepts `rhs`
    /// (or, for unary operators, `rhs == None`).
    pub(crate) fn lookup_op(
        &mut self,
        trait_name: &str,
        lhs: &Ty,
        rhs: Option<&Ty>,
        span: Span,
    ) -> Option<(FnId, Inst)> {
        let tr = self.prog.find_trait(trait_name)?;
        let lhs = self.infer.shallow(lhs);
        // generic code: `T: Add` bound
        if let Ty::Param(g) = &lhs {
            if self.prog.generic(*g).bounds.iter().any(|b| b.trait_ == tr) {
                let (_, fid) = self.prog.trait_(tr).methods[0].clone();
                let subst = vec![(self.prog.trait_(tr).self_generic, lhs.clone())];
                let inst = self.instantiate(fid, &subst, None, span);
                return Some((fid, inst));
            }
            return None;
        }
        for i in 0..self.prog.impls.len() {
            let imp = self.prog.impls[i].clone();
            if imp.trait_ref.as_ref().map(|t| t.trait_) != Some(tr) || imp.methods.is_empty() {
                continue;
            }
            let (_, fid) = imp.methods[0].clone();
            let snap = self.infer.snapshot();
            let subst = self.fresh_subst(&imp.generics, span);
            let mut ok = self.infer.unify(&imp.self_ty.subst(&subst), &lhs);
            let fsig = self.prog.fn_(fid).sig.clone();
            if ok {
                if let Some(r) = rhs {
                    ok = fsig.params.first().map_or(false, |p| {
                        let want = p.ty.subst(&subst);
                        self.infer.unify(&want, r)
                    });
                }
            }
            if ok {
                let inst = self.instantiate(fid, &subst, None, span);
                return Some((fid, inst));
            }
            self.infer.rollback(snap);
        }
        None
    }

    // ------------------------------------------------------------ instantiation

    /// Instantiate a function's generics (the enclosing impl/trait generics come in
    /// `parent`). `explicit` supplies the function's own type arguments, if written.
    pub(crate) fn instantiate(
        &mut self,
        fid: FnId,
        parent: &[(GenericId, Ty)],
        explicit: Option<&[Ty]>,
        span: Span,
    ) -> Inst {
        let sig = self.prog.fn_(fid).sig.clone();
        let mut subst: Vec<(GenericId, Ty)> = parent.to_vec();
        match explicit {
            Some(args) if args.len() == sig.generics.len() => {
                for (g, a) in sig.generics.iter().zip(args) {
                    subst.push((*g, a.clone()));
                }
            }
            Some(args) => {
                let name = self.prog.fn_(fid).name.clone();
                self.err(
                    span,
                    format!("`{name}` takes {} type argument(s), but {} were given", sig.generics.len(), args.len()),
                );
                for g in &sig.generics {
                    subst.push((*g, Ty::Error));
                }
            }
            None => {
                for g in &sig.generics {
                    let v = self.fresh(span);
                    subst.push((*g, v));
                }
            }
        }
        self.add_bound_obligations(&sig.generics, &subst, span);
        Inst {
            self_mode: sig.self_mode,
            params: sig
                .params
                .iter()
                .map(|p| (p.name.clone(), p.mode, p.ty.subst(&subst)))
                .collect(),
            ret: sig.ret.subst(&subst),
            subst,
        }
    }
}
