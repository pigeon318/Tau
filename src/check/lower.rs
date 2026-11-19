//! Lowering syntactic types (`TypeExpr`) to semantic types (`Ty`), and resolving
//! type names.

use super::*;

/// What a type name refers to.
pub(crate) enum TyHead {
    Prim(Ty),
    Param(GenericId),
    SelfTy(Ty),
    Adt(AdtId),
    Alias(usize),
    Trait(TraitId),
    Tensor,
}

pub(crate) fn prim_type(name: &str) -> Option<Ty> {
    if let Some(i) = IntTy::from_name(name) {
        return Some(Ty::Int(i));
    }
    Some(match name {
        "f32" => Ty::Float(FloatTy::F32),
        "f64" => Ty::Float(FloatTy::F64),
        "c64" => Ty::Complex(FloatTy::F32),
        "c128" => Ty::Complex(FloatTy::F64),
        "bool" => Ty::Bool,
        "char" => Ty::Char,
        "str" => Ty::Str,
        "Never" => Ty::Never,
        _ => return None,
    })
}

impl<'a> Checker<'a> {
    pub(crate) fn lookup_type(&self, module: usize, name: &str) -> Option<TypeItem> {
        self.scopes[module]
            .types
            .get(name)
            .or_else(|| self.scopes[0].types.get(name))
            .copied()
    }

    pub(crate) fn lookup_value(&self, module: usize, name: &str) -> Option<ValueItem> {
        self.scopes[module]
            .values
            .get(name)
            .or_else(|| self.scopes[0].values.get(name))
            .copied()
    }

    /// Find the module named by the first `n` segments of a path, if any.
    pub(crate) fn module_of_path(&self, module: usize, segs: &[&str]) -> Option<usize> {
        if segs.is_empty() {
            return None;
        }
        if let Some(m) = self.find_module(module, &segs.join(".")) {
            return Some(m);
        }
        let first = self.scopes[module].modules.get(segs[0]).copied()?;
        if segs.len() == 1 {
            return Some(first);
        }
        let path = format!("{}.{}", self.mods[first].0, segs[1..].join("."));
        self.mod_by_path.get(&path).copied()
    }

    pub(crate) fn declare_generics(&mut self, params: &[GenericParam]) -> Vec<(String, GenericId)> {
        params
            .iter()
            .map(|p| (p.name.name.clone(), self.prog.new_generic(&p.name.name)))
            .collect()
    }

    pub(crate) fn lower_bounds(
        &mut self,
        params: &[GenericParam],
        ids: &[(String, GenericId)],
        cx: &LowerCx,
    ) {
        for (p, (_, id)) in params.iter().zip(ids) {
            for b in &p.bounds {
                if let Some(tr) = self.lower_trait_ref(b, cx) {
                    self.prog.generics[id.idx()].bounds.push(tr);
                }
            }
        }
    }

    pub(crate) fn resolve_type_head(
        &mut self,
        segs: &[Ident],
        cx: &LowerCx,
        span: Span,
    ) -> Option<TyHead> {
        if segs.len() == 1 {
            let name = segs[0].name.as_str();
            if let Some((_, g)) = cx.generics.iter().rev().find(|(n, _)| n == name) {
                return Some(TyHead::Param(*g));
            }
            if name == "Self" {
                return match &cx.self_ty {
                    Some(t) => Some(TyHead::SelfTy(t.clone())),
                    None => {
                        self.err(span, "`Self` can only be used inside an `impl` or `trait`");
                        None
                    }
                };
            }
            if let Some(t) = prim_type(name) {
                return Some(TyHead::Prim(t));
            }
            if name == "Tensor" {
                return Some(TyHead::Tensor);
            }
            return match self.lookup_type(cx.module, name) {
                Some(TypeItem::Adt(a)) => Some(TyHead::Adt(a)),
                Some(TypeItem::Alias(a)) => Some(TyHead::Alias(a)),
                Some(TypeItem::Trait(t)) => Some(TyHead::Trait(t)),
                None => {
                    self.err_hint(
                        span,
                        format!("cannot find type `{name}` in this scope"),
                        "check the spelling, or import it with `use`",
                    );
                    None
                }
            };
        }
        let prefix: Vec<&str> = segs[..segs.len() - 1].iter().map(|s| s.name.as_str()).collect();
        let last = segs.last().unwrap().name.as_str();
        let Some(m) = self.module_of_path(cx.module, &prefix) else {
            self.err(span, format!("cannot find module `{}`", prefix.join(".")));
            return None;
        };
        match self.type_in(cx.module, m, last) {
            Err(()) => {
                self.private_item(span, last, m);
                None
            }
            Ok(Some(TypeItem::Adt(a))) => Some(TyHead::Adt(a)),
            Ok(Some(TypeItem::Alias(a))) => Some(TyHead::Alias(a)),
            Ok(Some(TypeItem::Trait(t))) => Some(TyHead::Trait(t)),
            Ok(None) => {
                self.err(
                    span,
                    format!("module `{}` has no type `{last}`", prefix.join(".")),
                );
                None
            }
        }
    }

    pub(crate) fn lower_ty(&mut self, t: &TypeExpr, cx: &LowerCx) -> Ty {
        match &t.kind {
            TypeKind::Infer => {
                if cx.allow_infer {
                    self.fresh(t.span)
                } else {
                    self.err(t.span, "the placeholder `_` is not allowed in this position");
                    Ty::Error
                }
            }
            TypeKind::Tuple(ts) => Ty::Tuple(ts.iter().map(|x| self.lower_ty(x, cx)).collect()),
            TypeKind::Slice(e) => Ty::Slice(Box::new(self.lower_ty(e, cx))),
            TypeKind::Array { elem, len } => {
                let n = match &len.kind {
                    ExprKind::Int { value, .. } => Some(*value),
                    _ => None,
                };
                if let Some(n) = n {
                    if n > MAX_ARRAY_LEN {
                        self.err_hint(len.span, format!("an array can have at most {MAX_ARRAY_LEN} elements, not {n}"), "use a `Vec` for more");
                    }
                }
                Ty::Array(Box::new(self.lower_ty(elem, cx)), n)
            }
            TypeKind::Fn { params, ret } => Ty::Fn(
                params.iter().map(|x| self.lower_ty(x, cx)).collect(),
                Box::new(match ret {
                    Some(r) => self.lower_ty(r, cx),
                    None => Ty::unit(),
                }),
            ),
            TypeKind::Dims(_) => {
                self.err(t.span, "a tensor shape like `[3, 3]` is only valid inside `Tensor<T, ...>`");
                Ty::Error
            }
            TypeKind::Path { segments, args } => self.lower_path_ty(segments, Some(args), t.span, cx, false),
        }
    }

    pub(crate) fn lower_dims(&mut self, t: &TypeExpr) -> Option<Vec<Dim>> {
        match &t.kind {
            TypeKind::Dims(ds) => Some(
                ds.iter()
                    .map(|d| match d {
                        crate::ast::Dim::Fixed(n) => Dim::Fixed(*n),
                        crate::ast::Dim::Dynamic => Dim::Dyn,
                        crate::ast::Dim::Named(i) => Dim::Named(i.name.clone()),
                    })
                    .collect(),
            ),
            // `[N]` is parsed as a one-element slice of the type `N`
            TypeKind::Slice(inner) => match &inner.kind {
                TypeKind::Path { segments, args } if segments.len() == 1 && args.is_empty() => {
                    Some(vec![Dim::Named(segments[0].name.clone())])
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Lower a (possibly generic) type path. `args` is `None` when no argument list
    /// was written. With `infer_missing`, omitted arguments become inference
    /// variables (used for `Vec.new()`).
    pub(crate) fn lower_path_ty(
        &mut self,
        segs: &[Ident],
        args: Option<&[TypeExpr]>,
        span: Span,
        cx: &LowerCx,
        infer_missing: bool,
    ) -> Ty {
        let Some(head) = self.resolve_type_head(segs, cx, span) else {
            return Ty::Error;
        };
        let given = args.unwrap_or(&[]);
        let no_args = |this: &mut Self, what: &str| {
            if !given.is_empty() {
                this.err(span, format!("{what} does not take type arguments"));
            }
        };
        match head {
            TyHead::Prim(t) => {
                no_args(self, "this type");
                t
            }
            TyHead::Param(g) => {
                no_args(self, "a type parameter");
                Ty::Param(g)
            }
            TyHead::SelfTy(t) => {
                no_args(self, "`Self`");
                t
            }
            TyHead::Trait(_) => {
                self.err_hint(
                    span,
                    format!("`{}` is a trait, not a type", segs.last().unwrap().name),
                    "use it as a bound, like `T: Trait`",
                );
                Ty::Error
            }
            TyHead::Tensor => {
                if given.len() == 2 {
                    let elem = self.lower_ty(&given[0], cx);
                    if !matches!(elem, Ty::Float(_) | Ty::Complex(_) | Ty::Bool | Ty::Param(_) | Ty::Error | Ty::Var(_))
                        && !matches!(elem, Ty::Int(i) if i.bits() <= 64)
                    {
                        let shown = self.show(&elem);
                        let hint = if matches!(elem, Ty::Int(_)) { "tensors hold integers up to 64 bits" } else { "tensors hold numbers (integers, floats, complex numbers) and `bool`" };
                        self.err_hint(given[0].span, format!("`{shown}` cannot be the element type of a tensor"), hint);
                        return Ty::Error;
                    }
                    match self.lower_dims(&given[1]) {
                        Some(dims) => Ty::Tensor(Box::new(elem), dims),
                        None => {
                            self.err(given[1].span, "expected a shape like `[3, 3]` or `[_, _]`");
                            Ty::Error
                        }
                    }
                } else if given.is_empty() && infer_missing {
                    self.err(span, "a tensor type needs an element type and shape, like `Tensor<f64, [3, 3]>`");
                    Ty::Error
                } else {
                    self.err(
                        span,
                        "`Tensor` takes an element type and a shape, like `Tensor<f64, [3, 3]>`",
                    );
                    Ty::Error
                }
            }
            TyHead::Adt(id) => {
                let n = self.prog.adt(id).generics.len();
                let lowered: Vec<Ty> = given.iter().map(|a| self.lower_ty(a, cx)).collect();
                if lowered.len() == n {
                    Ty::Adt(id, lowered)
                } else if lowered.is_empty() && infer_missing {
                    let vars = (0..n).map(|_| self.fresh(span)).collect();
                    Ty::Adt(id, vars)
                } else {
                    let name = self.prog.adt(id).name.clone();
                    self.err(
                        span,
                        format!("`{name}` expects {n} type argument(s), but {} were given", lowered.len()),
                    );
                    Ty::Error
                }
            }
            TyHead::Alias(i) => {
                let generics = self.alias_generics(i);
                let Some(body) = self.alias_ty(i, span) else {
                    return Ty::Error;
                };
                let lowered: Vec<Ty> = given.iter().map(|a| self.lower_ty(a, cx)).collect();
                if lowered.len() != generics.len() {
                    self.err(span, "wrong number of type arguments for this type alias");
                    return Ty::Error;
                }
                let map: Vec<(GenericId, Ty)> =
                    generics.iter().map(|(_, g)| *g).zip(lowered).collect();
                body.subst(&map)
            }
        }
    }

    pub(crate) fn lower_alias(&mut self, i: usize) {
        let span = self.aliases[i].decl.name.span;
        self.alias_ty(i, span);
    }

    fn alias_generics(&self, i: usize) -> Vec<(String, GenericId)> {
        self.aliases[i].generics.clone()
    }

    /// The lowered body of an alias (lowered on first use; cycles are errors).
    fn alias_ty(&mut self, i: usize, span: Span) -> Option<Ty> {
        if let Some(t) = &self.aliases[i].ty {
            return Some(t.clone());
        }
        if self.aliases[i].resolving {
            self.err(span, "this type alias refers to itself");
            return None;
        }
        self.aliases[i].resolving = true;
        let decl = self.aliases[i].decl;
        let cx = LowerCx {
            module: self.aliases[i].module,
            generics: self.aliases[i].generics.clone(),
            ..LowerCx::default()
        };
        let t = self.lower_ty(&decl.ty, &cx);
        self.aliases[i].resolving = false;
        self.aliases[i].ty = Some(t.clone());
        Some(t)
    }

    pub(crate) fn lower_trait_ref(&mut self, t: &TypeExpr, cx: &LowerCx) -> Option<TraitRef> {
        let TypeKind::Path { segments, args } = &t.kind else {
            self.err(t.span, "expected a trait name");
            return None;
        };
        match self.resolve_type_head(segments, cx, t.span)? {
            TyHead::Trait(id) => {
                let n = self.prog.trait_(id).generics.len();
                let lowered: Vec<Ty> = args.iter().map(|a| self.lower_ty(a, cx)).collect();
                if lowered.len() != n {
                    let name = self.prog.trait_(id).name.clone();
                    self.err(
                        t.span,
                        format!("trait `{name}` expects {n} type argument(s), but {} were given", lowered.len()),
                    );
                    return None;
                }
                Some(TraitRef {
                    trait_: id,
                    args: lowered,
                })
            }
            _ => {
                self.err(
                    t.span,
                    format!("`{}` is not a trait", segments.last().unwrap().name),
                );
                None
            }
        }
    }
}
