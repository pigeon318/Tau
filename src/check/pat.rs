//! Patterns and match exhaustiveness.

use super::*;

impl<'a> Checker<'a> {
    /// Resolve a path used in a pattern to an enum variant or struct constructor.
    fn pattern_ctor(&mut self, path: &[Ident], span: Span) -> Option<ValueItem> {
        let module = self.body.cx.module;
        if path.len() == 1 {
            return match self.lookup_value(module, &path[0].name) {
                Some(v @ (ValueItem::Variant(..) | ValueItem::StructCtor(_))) => Some(v),
                _ => {
                    self.err_hint(
                        span,
                        format!("`{}` is not an enum variant or struct", path[0].name),
                        "patterns can destructure variants like `Some(x)` and structs",
                    );
                    None
                }
            };
        }
        // `Enum.Variant`
        let enum_name = &path[path.len() - 2].name;
        let variant = &path[path.len() - 1].name;
        let adt = match self.lookup_type(module, enum_name) {
            Some(TypeItem::Adt(a)) => a,
            _ => {
                self.err(span, format!("cannot find enum `{enum_name}`"));
                return None;
            }
        };
        if let AdtKind::Enum(vs) = &self.prog.adt(adt).kind {
            if let Some(i) = vs.iter().position(|v| v.name == *variant) {
                return Some(ValueItem::Variant(adt, i));
            }
        }
        self.err(span, format!("enum `{enum_name}` has no variant `{variant}`"));
        None
    }

    /// Does this bare name in a pattern refer to a unit variant (rather than bind)?
    fn is_unit_variant(&self, name: &str) -> Option<(AdtId, usize)> {
        match self.lookup_value(self.body.cx.module, name) {
            Some(ValueItem::Variant(a, i)) => match &self.prog.adt(a).kind {
                AdtKind::Enum(vs) if vs[i].fields.is_empty() => Some((a, i)),
                _ => None,
            },
            _ => None,
        }
    }

    pub(super) fn pattern(&mut self, p: &'a Pattern, ty: &Ty, force_mut: bool) {
        let key = NodeKey::of(p);
        self.results.pat_ty.insert(key, ty.clone());
        self.body.touched_pats.push(key);
        match &p.kind {
            PatKind::Wild => {}
            PatKind::Ident { name, mutable } => {
                if !*mutable {
                    if let Some((adt, _)) = self.is_unit_variant(name) {
                        let args = self.fresh_adt_args(adt, p.span);
                        let at = Ty::Adt(adt, args);
                        self.coerce(&at, ty, p.span);
                        return;
                    }
                }
                self.declare_local(name, ty.clone(), *mutable || force_mut, p.span);
            }
            PatKind::Lit(e) => {
                let t = self.expr(e, Some(ty));
                self.coerce(&t, ty, p.span);
            }
            PatKind::Range { lo, hi, .. } => {
                let a = self.expr(lo, Some(ty));
                self.coerce(&a, ty, p.span);
                let b = self.expr(hi, Some(ty));
                self.coerce(&b, ty, p.span);
            }
            PatKind::Tuple(ps) => {
                let st = self.infer.shallow(ty);
                let elems: Vec<Ty> = match &st {
                    Ty::Tuple(ts) if ts.len() == ps.len() => ts.clone(),
                    Ty::Var(_) => {
                        let vars: Vec<Ty> = ps.iter().map(|_| self.fresh(p.span)).collect();
                        self.infer.unify(&st, &Ty::Tuple(vars.clone()));
                        vars
                    }
                    Ty::Error => ps.iter().map(|_| Ty::Error).collect(),
                    other => {
                        let found = self.show(other);
                        self.err(
                            p.span,
                            format!("mismatched types: this pattern has {} element(s), but the value is `{found}`", ps.len()),
                        );
                        ps.iter().map(|_| Ty::Error).collect()
                    }
                };
                for (sub, t) in ps.iter().zip(elems) {
                    self.pattern(sub, &t, force_mut);
                }
            }
            PatKind::Variant { path, args } => {
                let Some(item) = self.pattern_ctor(path, p.span) else {
                    self.error_bindings(p);
                    return;
                };
                let (adt, field_tys) = self.ctor_fields(item, ty, p.span);
                if field_tys.len() != args.len() {
                    let name = &path.last().unwrap().name;
                    self.err(
                        p.span,
                        format!("`{name}` has {} field(s), but the pattern has {}", field_tys.len(), args.len()),
                    );
                    self.error_bindings(p);
                    return;
                }
                let _ = adt;
                for (sub, t) in args.iter().zip(field_tys) {
                    self.pattern(sub, &t, force_mut);
                }
            }
            PatKind::Path(path) => match self.pattern_ctor(path, p.span) {
                Some(item) => {
                    let (_, fields) = self.ctor_fields(item, ty, p.span);
                    if !fields.is_empty() {
                        self.err(p.span, "this variant has fields; write `Variant(..)` with a pattern for each");
                    }
                }
                None => {}
            },
            PatKind::Or(alts) => {
                let first = pattern_names(&alts[0]);
                for alt in &alts[1..] {
                    let mut names = pattern_names(alt);
                    let mut f = first.clone();
                    names.sort();
                    f.sort();
                    if names != f {
                        self.err(alt.span, "every alternative of an `|` pattern must bind the same variables");
                    }
                }
                self.pattern(&alts[0], ty, force_mut);
                for alt in &alts[1..] {
                    self.push_scope();
                    self.pattern(alt, ty, force_mut);
                    self.pop_scope();
                }
            }
        }
    }

    /// After an error in a pattern, still declare its names so later code does not
    /// produce a cascade of "cannot find value" errors.
    fn error_bindings(&mut self, p: &'a Pattern) {
        for n in pattern_names(p) {
            self.declare_local(&n, Ty::Error, true, p.span);
        }
    }

    pub(super) fn fresh_adt_args(&mut self, adt: AdtId, span: Span) -> Vec<Ty> {
        let n = self.prog.adt(adt).generics.len();
        (0..n).map(|_| self.fresh(span)).collect()
    }

    /// For a constructor pattern against a value of type `ty`: unify `ty` with the
    /// constructor's type and return the field types.
    fn ctor_fields(&mut self, item: ValueItem, ty: &Ty, span: Span) -> (AdtId, Vec<Ty>) {
        let (adt, fields_raw): (AdtId, Vec<Ty>) = match item {
            ValueItem::Variant(a, i) => match &self.prog.adt(a).kind {
                AdtKind::Enum(vs) => (a, vs[i].fields.clone()),
                _ => (a, vec![]),
            },
            ValueItem::StructCtor(a) => match &self.prog.adt(a).kind {
                AdtKind::Struct(fs) => (a, fs.iter().map(|f| f.ty.clone()).collect()),
                _ => (a, vec![]),
            },
            _ => unreachable!(),
        };
        let gens = self.prog.adt(adt).generics.clone();
        let args = self.fresh_adt_args(adt, span);
        let subst: Vec<(GenericId, Ty)> = gens.into_iter().zip(args.clone()).collect();
        self.coerce(&Ty::Adt(adt, args), ty, span);
        (adt, fields_raw.iter().map(|t| t.subst(&subst)).collect())
    }

    // ------------------------------------------------------------ exhaustiveness

    fn irrefutable(&self, p: &Pattern) -> bool {
        match &p.kind {
            PatKind::Wild => true,
            PatKind::Ident { name, mutable } => *mutable || self.is_unit_variant(name).is_none(),
            PatKind::Tuple(ps) => ps.iter().all(|x| self.irrefutable(x)),
            PatKind::Or(alts) => alts.iter().any(|x| self.irrefutable(x)),
            PatKind::Variant { path, args } => {
                // a struct pattern with irrefutable fields, or a one-variant enum
                if path.len() == 1 {
                    if let Some(ValueItem::StructCtor(_)) = self.lookup_value(self.body.cx.module, &path[0].name) {
                        return args.iter().all(|x| self.irrefutable(x));
                    }
                }
                false
            }
            _ => false,
        }
    }

    fn covered_variants(&self, p: &Pattern, out: &mut Vec<usize>) {
        match &p.kind {
            PatKind::Ident { name, mutable: false } => {
                if let Some((_, i)) = self.is_unit_variant(name) {
                    out.push(i);
                }
            }
            PatKind::Variant { path, .. } | PatKind::Path(path) => {
                let module = self.body.cx.module;
                if path.len() == 1 {
                    if let Some(ValueItem::Variant(_, i)) = self.lookup_value(module, &path[0].name) {
                        out.push(i);
                    }
                } else if path.len() >= 2 {
                    let en = &path[path.len() - 2].name;
                    let vn = &path[path.len() - 1].name;
                    if let Some(TypeItem::Adt(a)) = self.lookup_type(module, en) {
                        if let AdtKind::Enum(vs) = &self.prog.adt(a).kind {
                            if let Some(i) = vs.iter().position(|v| v.name == *vn) {
                                out.push(i);
                            }
                        }
                    }
                }
            }
            PatKind::Or(alts) => alts.iter().for_each(|a| self.covered_variants(a, out)),
            _ => {}
        }
    }

    pub(super) fn check_exhaustive(&mut self, scrut: &Ty, arms: &'a [MatchArm], span: Span) {
        let ty = self.infer.resolve(scrut);
        if matches!(ty, Ty::Error | Ty::Var(_) | Ty::Never) {
            return;
        }
        if arms
            .iter()
            .any(|a| a.guard.is_none() && self.irrefutable(&a.pat))
        {
            return;
        }
        match &ty {
            Ty::Adt(id, _) if matches!(self.prog.adt(*id).kind, AdtKind::Enum(_)) => {
                let mut covered = Vec::new();
                for a in arms.iter().filter(|a| a.guard.is_none()) {
                    self.covered_variants(&a.pat, &mut covered);
                }
                if let AdtKind::Enum(vs) = &self.prog.adt(*id).kind {
                    let missing: Vec<String> = vs
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| !covered.contains(i))
                        .map(|(_, v)| format!("`{}`", v.name))
                        .collect();
                    if !missing.is_empty() {
                        self.err_hint(
                            span,
                            format!("non-exhaustive match: missing {}", missing.join(", ")),
                            "add arms for the missing variants, or a `_` arm",
                        );
                    }
                }
            }
            Ty::Bool => {
                let mut t = false;
                let mut f = false;
                for a in arms.iter().filter(|a| a.guard.is_none()) {
                    let mut lits = vec![&a.pat];
                    if let PatKind::Or(alts) = &a.pat.kind {
                        lits = alts.iter().collect();
                    }
                    for p in lits {
                        if let PatKind::Lit(e) = &p.kind {
                            if let ExprKind::Bool(b) = e.kind {
                                if b { t = true } else { f = true }
                            }
                        }
                    }
                }
                if !(t && f) {
                    self.err_hint(span, "non-exhaustive match on `bool`", "cover both `true` and `false`, or add a `_` arm");
                }
            }
            _ => {
                self.err_hint(
                    span,
                    "non-exhaustive match: not every value is covered",
                    "add a final `_ => ...` arm",
                );
            }
        }
    }
}

/// The variable names a pattern binds (best effort; unit variants are filtered later).
pub(super) fn pattern_names(p: &Pattern) -> Vec<String> {
    match &p.kind {
        PatKind::Ident { name, .. } => vec![name.clone()],
        PatKind::Tuple(ps) => ps.iter().flat_map(pattern_names).collect(),
        PatKind::Variant { args, .. } => args.iter().flat_map(pattern_names).collect(),
        PatKind::Or(alts) => alts.first().map(pattern_names).unwrap_or_default(),
        _ => vec![],
    }
}
