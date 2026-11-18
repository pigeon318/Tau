//! Passes 1 and 2: register names, process imports, lower signatures, and check
//! that impls conform to their traits.

use super::*;

fn has_attr(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().any(|a| a.name.name == name)
}

impl<'a> Checker<'a> {
    // ------------------------------------------------------------ registration

    pub(super) fn collect_items(&mut self) {
        for m in 0..self.mods.len() {
            let module: &'a Module = self.mods[m].1;
            for item in &module.items {
                self.collect_item(m, item);
            }
        }
    }

    fn define_type(&mut self, m: usize, name: &Ident, item: TypeItem) {
        if self.scopes[m].types.contains_key(&name.name) {
            self.err(name.span, format!("the name `{}` is defined multiple times", name.name));
        } else {
            self.scopes[m].types.insert(name.name.clone(), item);
        }
    }

    fn define_value(&mut self, m: usize, name: &Ident, item: ValueItem) {
        if self.scopes[m].values.contains_key(&name.name) {
            self.err_hint(
                name.span,
                format!("the name `{}` is defined multiple times", name.name),
                "enum variants share one namespace with functions and constants",
            );
        } else {
            self.scopes[m].values.insert(name.name.clone(), item);
        }
    }

    pub(super) fn new_fn(&mut self, f: &'a FnDecl, module: usize, public: bool, parent: Parent) -> FnId {
        self.prog.fns.push(FnDef {
            name: f.name.name.clone(),
            sig: FnSig {
                generics: Vec::new(),
                self_mode: None,
                params: Vec::new(),
                ret: Ty::unit(),
            },
            parent,
            module,
            public,
            is_unsafe: f.is_unsafe,
            has_body: f.body.is_some(),
        });
        self.fn_decls.push(Some(f));
        self.fn_cx.push(LowerCx {
            module,
            ..LowerCx::default()
        });
        let id = FnId(self.prog.fns.len() as u32 - 1);
        self.results.fn_decl.insert(NodeKey::of(f), id);
        id
    }

    fn collect_item(&mut self, m: usize, item: &'a Item) {
        match &item.kind {
            ItemKind::Fn(f) => {
                let id = self.new_fn(f, m, item.public, Parent::Free);
                self.define_value(m, &f.name, ValueItem::Fn(id));
            }
            ItemKind::Struct(s) => {
                let gens = self.declare_generics(&s.generics);
                self.prog.adts.push(AdtDef {
                    name: s.name.name.clone(),
                    generics: gens.iter().map(|g| g.1).collect(),
                    kind: AdtKind::Struct(Vec::new()),
                    module: m,
                    public: item.public,
                });
                self.adt_srcs.push(AdtSrc::Struct(s));
                let id = AdtId(self.prog.adts.len() as u32 - 1);
                self.define_type(m, &s.name, TypeItem::Adt(id));
                self.define_value(m, &s.name, ValueItem::StructCtor(id));
            }
            ItemKind::Enum(e) => {
                let gens = self.declare_generics(&e.generics);
                self.prog.adts.push(AdtDef {
                    name: e.name.name.clone(),
                    generics: gens.iter().map(|g| g.1).collect(),
                    kind: AdtKind::Enum(Vec::new()),
                    module: m,
                    public: item.public,
                });
                self.adt_srcs.push(AdtSrc::Enum(e));
                let id = AdtId(self.prog.adts.len() as u32 - 1);
                self.define_type(m, &e.name, TypeItem::Adt(id));
                for (i, v) in e.variants.iter().enumerate() {
                    self.define_value(m, &v.name, ValueItem::Variant(id, i));
                }
            }
            ItemKind::Trait(t) => {
                let self_generic = self.prog.new_generic("Self");
                let gens = self.declare_generics(&t.generics);
                self.prog.traits.push(TraitDef {
                    name: t.name.name.clone(),
                    self_generic,
                    generics: gens.iter().map(|g| g.1).collect(),
                    methods: Vec::new(),
                    is_op: has_attr(&item.attrs, "op"),
                    builtin_rule: has_attr(&item.attrs, "builtin_trait"),
                    module: m,
                });
                self.trait_srcs.push(t);
                self.trait_attrs.push(&item.attrs);
                let id = TraitId(self.prog.traits.len() as u32 - 1);
                self.define_type(m, &t.name, TypeItem::Trait(id));
            }
            ItemKind::Impl(i) => self.impl_srcs.push((i, m)),
            ItemKind::Const(c) => {
                self.prog.consts.push(ConstDef {
                    name: c.name.name.clone(),
                    ty: Ty::Error,
                    module: m,
                    public: item.public,
                });
                self.const_srcs.push(Some(c));
                let id = ConstId(self.prog.consts.len() as u32 - 1);
                self.results.const_decl.insert(NodeKey::of(c), id);
                self.define_value(m, &c.name, ValueItem::Const(id));
            }
            ItemKind::TypeAlias(a) => {
                let generics = self.declare_generics(&a.generics);
                self.aliases.push(AliasEntry {
                    decl: a,
                    module: m,
                    generics,
                    ty: None,
                    resolving: false,
                });
                let idx = self.aliases.len() - 1;
                self.define_type(m, &a.name, TypeItem::Alias(idx));
            }
            ItemKind::Cluster(c) => {
                self.prog.consts.push(ConstDef {
                    name: c.name.name.clone(),
                    ty: Ty::Error,
                    module: m,
                    public: item.public,
                });
                self.const_srcs.push(None);
                let id = ConstId(self.prog.consts.len() as u32 - 1);
                self.define_value(m, &c.name, ValueItem::Const(id));
            }
            ItemKind::Use(_) => {}
        }
    }

    // ------------------------------------------------------------ imports

    pub(super) fn resolve_imports(&mut self) {
        for m in 0..self.mods.len() {
            let module: &'a Module = self.mods[m].1;
            for item in &module.items {
                if let ItemKind::Use(tree) = &item.kind {
                    self.resolve_use(m, tree, &[]);
                }
            }
        }
    }

    fn resolve_use(&mut self, m: usize, tree: &UseTree, prefix: &[String]) {
        let mut full: Vec<String> = prefix.to_vec();
        full.extend(tree.path.iter().map(|i| i.name.clone()));
        let joined = full.join(".");
        match &tree.kind {
            UseKind::Group(trees) => {
                for t in trees {
                    self.resolve_use(m, t, &full);
                }
            }
            UseKind::Glob => match self.mod_by_path.get(&joined).copied() {
                Some(src) => {
                    let types: Vec<_> = self.scopes[src].types.iter().map(|(k, v)| (k.clone(), *v)).collect();
                    let values: Vec<_> = self.scopes[src].values.iter().map(|(k, v)| (k.clone(), *v)).collect();
                    for (k, v) in types {
                        self.scopes[m].types.entry(k).or_insert(v);
                    }
                    for (k, v) in values {
                        self.scopes[m].values.entry(k).or_insert(v);
                    }
                }
                None => self.unresolved_import(tree.span, &joined),
            },
            UseKind::Simple { alias } => {
                let last = full.last().unwrap().clone();
                let local = alias.as_ref().map(|a| a.name.clone()).unwrap_or(last.clone());
                // a whole module
                if let Some(src) = self.mod_by_path.get(&joined).copied() {
                    self.scopes[m].modules.insert(local, src);
                    return;
                }
                // an item inside a module
                let parent = full[..full.len() - 1].join(".");
                if let Some(src) = self.mod_by_path.get(&parent).copied() {
                    let mut found = false;
                    if let Some(t) = self.scopes[src].types.get(&last).copied() {
                        self.scopes[m].types.entry(local.clone()).or_insert(t);
                        found = true;
                    }
                    if let Some(v) = self.scopes[src].values.get(&last).copied() {
                        self.scopes[m].values.entry(local.clone()).or_insert(v);
                        found = true;
                    }
                    if found {
                        return;
                    }
                }
                self.unresolved_import(tree.span, &joined);
            }
        }
    }

    fn unresolved_import(&mut self, span: Span, path: &str) {
        let mut avail: Vec<&str> = self
            .mods
            .iter()
            .map(|(p, _)| p.as_str())
            .filter(|p| *p != "prelude" && *p != "main")
            .collect();
        avail.sort();
        self.err_hint(
            span,
            format!("unresolved import `{path}`"),
            format!("available modules: {}", avail.join(", ")),
        );
    }

    // ------------------------------------------------------------ signatures

    pub(super) fn lower_signatures(&mut self) {
        for i in 0..self.aliases.len() {
            self.lower_alias(i);
        }
        for i in 0..self.adt_srcs.len() {
            self.lower_adt(AdtId(i as u32));
        }
        for i in 0..self.trait_srcs.len() {
            self.lower_trait(TraitId(i as u32));
        }
        for i in 0..self.impl_srcs.len() {
            self.lower_impl(i);
        }
        for i in 0..self.prog.fns.len() {
            if self.prog.fns[i].parent == Parent::Free {
                let decl = self.fn_decls[i].unwrap();
                let base = LowerCx {
                    module: self.prog.fns[i].module,
                    ..LowerCx::default()
                };
                let (sig, cx) = self.lower_fn_sig(decl, &base);
                self.prog.fns[i].sig = sig;
                self.fn_cx[i] = cx;
            }
        }
        let cluster = self.prog.find_adt("Cluster");
        for i in 0..self.prog.consts.len() {
            match self.const_srcs[i] {
                Some(decl) => {
                    let cx = LowerCx {
                        module: self.prog.consts[i].module,
                        ..LowerCx::default()
                    };
                    self.prog.consts[i].ty = self.lower_ty(&decl.ty, &cx);
                }
                None => {
                    self.prog.consts[i].ty = cluster.map(|c| Ty::Adt(c, vec![])).unwrap_or(Ty::Error);
                }
            }
        }
        for i in 0..self.impl_srcs.len() {
            self.check_conformance(i);
        }
        self.check_recursive_types();
    }

    /// Does a value of `ty` contain a value of the type `target` *inside itself* (not behind
    /// a vector, string, map, slice, pointer, tensor or function, which store their contents
    /// elsewhere)? If a type contains itself it would have to be infinitely large.
    fn contains_by_value(&self, ty: &Ty, target: AdtId, seen: &mut Vec<AdtId>) -> bool {
        match ty {
            Ty::Adt(id, args) => {
                if *id == target {
                    return true;
                }
                if seen.contains(id) {
                    return false;
                }
                seen.push(*id);
                let def = self.prog.adt(*id);
                let subst: Vec<(GenericId, Ty)> = def.generics.iter().copied().zip(args.iter().cloned()).collect();
                match &def.kind {
                    AdtKind::Struct(fs) => fs.iter().any(|f| self.contains_by_value(&f.ty.subst(&subst), target, seen)),
                    AdtKind::Enum(vs) => vs
                        .iter()
                        .any(|v| v.fields.iter().any(|t| self.contains_by_value(&t.subst(&subst), target, seen))),
                }
            }
            Ty::Tuple(ts) => ts.iter().any(|t| self.contains_by_value(t, target, seen)),
            Ty::Array(t, _) => self.contains_by_value(t, target, seen),
            _ => false,
        }
    }

    fn check_recursive_types(&mut self) {
        for i in 0..self.adt_srcs.len() {
            let id = AdtId(i as u32);
            if !self.is_user(self.prog.adt(id).module) {
                continue;
            }
            let field_tys: Vec<Ty> = match &self.prog.adt(id).kind {
                AdtKind::Struct(fs) => fs.iter().map(|f| f.ty.clone()).collect(),
                AdtKind::Enum(vs) => vs.iter().flat_map(|v| v.fields.iter().cloned()).collect(),
            };
            if field_tys.iter().any(|t| self.contains_by_value(t, id, &mut Vec::new())) {
                let (name, span) = match self.adt_srcs[i] {
                    AdtSrc::Struct(s) => (s.name.name.clone(), s.name.span),
                    AdtSrc::Enum(e) => (e.name.name.clone(), e.name.span),
                };
                self.err_hint(
                    span,
                    format!("the type `{name}` contains itself, so it would have infinite size"),
                    "put the repeated part behind a `Vec` (for example a vector of children, or indices into a vector); a `Box` type is not available yet",
                );
            }
        }
    }

    fn adt_cx(&self, id: AdtId, params: &[GenericParam]) -> LowerCx {
        let def = self.prog.adt(id);
        let generics: Vec<(String, GenericId)> = params
            .iter()
            .zip(&def.generics)
            .map(|(p, g)| (p.name.name.clone(), *g))
            .collect();
        LowerCx {
            module: def.module,
            self_ty: Some(Ty::Adt(id, def.generics.iter().map(|g| Ty::Param(*g)).collect())),
            generics,
            allow_infer: false,
        }
    }

    fn lower_adt(&mut self, id: AdtId) {
        match self.adt_srcs[id.idx()] {
            AdtSrc::Struct(s) => {
                let cx = self.adt_cx(id, &s.generics);
                self.lower_bounds(&s.generics, &cx.generics.clone(), &cx);
                let mut fields: Vec<FieldDef> = Vec::new();
                for f in &s.fields {
                    if fields.iter().any(|x| x.name == f.name.name) {
                        self.err(f.name.span, format!("field `{}` is declared twice", f.name.name));
                        continue;
                    }
                    let ty = self.lower_ty(&f.ty, &cx);
                    fields.push(FieldDef {
                        name: f.name.name.clone(),
                        ty,
                        public: f.public,
                    });
                }
                self.prog.adts[id.idx()].kind = AdtKind::Struct(fields);
            }
            AdtSrc::Enum(e) => {
                let cx = self.adt_cx(id, &e.generics);
                self.lower_bounds(&e.generics, &cx.generics.clone(), &cx);
                let mut variants = Vec::new();
                for v in &e.variants {
                    let fields = v.fields.iter().map(|t| self.lower_ty(t, &cx)).collect();
                    variants.push(VariantDef {
                        name: v.name.name.clone(),
                        fields,
                    });
                }
                self.prog.adts[id.idx()].kind = AdtKind::Enum(variants);
            }
        }
    }

    /// Lower a function signature. `base` carries the generics and `Self` of the
    /// enclosing impl or trait. Returns the signature and the context for its body.
    pub(super) fn lower_fn_sig(&mut self, decl: &FnDecl, base: &LowerCx) -> (FnSig, LowerCx) {
        let own = self.declare_generics(&decl.generics);
        let mut cx = base.clone();
        cx.generics.extend(own.iter().cloned());
        self.lower_bounds(&decl.generics, &own, &cx);
        let mut self_mode = None;
        let mut params = Vec::new();
        for (i, p) in decl.params.iter().enumerate() {
            if p.name.name == "self" {
                if i != 0 {
                    self.err(p.span, "`self` must be the first parameter");
                } else if base.self_ty.is_none() {
                    self.err(p.span, "`self` is only allowed in methods inside an `impl` or `trait`");
                } else {
                    self_mode = Some(p.mode);
                }
                continue;
            }
            let ty = match &p.ty {
                Some(t) => self.lower_ty(t, &cx),
                None => Ty::Error,
            };
            params.push(ParamDef {
                name: p.name.name.clone(),
                mode: p.mode,
                ty,
            });
        }
        let ret = match &decl.ret {
            Some(t) => self.lower_ty(t, &cx),
            None => Ty::unit(),
        };
        let sig = FnSig {
            generics: own.iter().map(|g| g.1).collect(),
            self_mode,
            params,
            ret,
        };
        (sig, cx)
    }

    fn lower_trait(&mut self, id: TraitId) {
        let decl = self.trait_srcs[id.idx()];
        let def = self.prog.trait_(id).clone();
        let generics: Vec<(String, GenericId)> = decl
            .generics
            .iter()
            .zip(&def.generics)
            .map(|(p, g)| (p.name.name.clone(), *g))
            .collect();
        let cx = LowerCx {
            module: def.module,
            generics,
            self_ty: Some(Ty::Param(def.self_generic)),
            allow_infer: false,
        };
        self.lower_bounds(&decl.generics, &cx.generics.clone(), &cx);
        // `Self` implicitly satisfies the trait itself
        self.prog.generics[def.self_generic.idx()].bounds.push(TraitRef {
            trait_: id,
            args: def.generics.iter().map(|g| Ty::Param(*g)).collect(),
        });
        for item in &decl.items {
            match &item.kind {
                ItemKind::Fn(f) => {
                    let fid = self.new_fn(f, def.module, true, Parent::Trait(id));
                    let (sig, fcx) = self.lower_fn_sig(f, &cx);
                    self.prog.fns[fid.idx()].sig = sig;
                    self.fn_cx[fid.idx()] = fcx;
                    self.prog.traits[id.idx()].methods.push((f.name.name.clone(), fid));
                }
                _ => self.err(item.span, "only functions can be declared inside a trait"),
            }
        }
    }

    fn lower_impl(&mut self, idx: usize) {
        let (decl, m) = self.impl_srcs[idx];
        let gens = self.declare_generics(&decl.generics);
        let mut cx = LowerCx {
            module: m,
            generics: gens.clone(),
            self_ty: None,
            allow_infer: false,
        };
        self.lower_bounds(&decl.generics, &gens, &cx);
        let self_ty = self.lower_ty(&decl.self_ty, &cx);
        cx.self_ty = Some(self_ty.clone());
        let trait_ref = decl.trait_ref.as_ref().and_then(|t| self.lower_trait_ref(t, &cx));
        self.prog.impls.push(ImplDef {
            generics: gens.iter().map(|g| g.1).collect(),
            trait_ref,
            self_ty,
            methods: Vec::new(),
            module: m,
        });
        let id = ImplId(self.prog.impls.len() as u32 - 1);
        debug_assert_eq!(id.idx(), idx);
        for item in &decl.items {
            match &item.kind {
                ItemKind::Fn(f) => {
                    let fid = self.new_fn(f, m, item.public, Parent::Impl(id));
                    let (sig, fcx) = self.lower_fn_sig(f, &cx);
                    self.prog.fns[fid.idx()].sig = sig;
                    self.fn_cx[fid.idx()] = fcx;
                    if self.prog.impls[idx].methods.iter().any(|(n, _)| *n == f.name.name) {
                        self.err(f.name.span, format!("method `{}` is defined twice in this impl", f.name.name));
                    }
                    self.prog.impls[idx].methods.push((f.name.name.clone(), fid));
                }
                _ => self.err(item.span, "only functions can be defined inside an impl"),
            }
        }
    }

    // ------------------------------------------------------------ conformance

    fn check_conformance(&mut self, idx: usize) {
        let (decl, _) = self.impl_srcs[idx];
        let imp = self.prog.impls[idx].clone();
        let Some(tr) = imp.trait_ref.clone() else { return };
        let tdef = self.prog.trait_(tr.trait_).clone();
        let mut base: Vec<(GenericId, Ty)> = vec![(tdef.self_generic, imp.self_ty.clone())];
        for (g, a) in tdef.generics.iter().zip(&tr.args) {
            base.push((*g, a.clone()));
        }
        for (name, tfid) in &tdef.methods {
            let tfn = self.prog.fn_(*tfid).clone();
            let Some((_, ifid)) = imp.methods.iter().find(|(n, _)| n == name) else {
                if !tfn.has_body {
                    self.err_hint(
                        decl.self_ty.span,
                        format!("missing method `{name}` in the impl of `{}`", tdef.name),
                        format!("add `fn {name}(...)` to this impl"),
                    );
                }
                continue;
            };
            let ifn = self.prog.fn_(*ifid).clone();
            let span = self.fn_decls[ifid.idx()].map(|d| d.name.span).unwrap_or(decl.self_ty.span);
            if tfn.sig.self_mode.is_some() != ifn.sig.self_mode.is_some() {
                let msg = if tfn.sig.self_mode.is_some() {
                    format!("method `{name}` should take `self`, as declared in trait `{}`", tdef.name)
                } else {
                    format!("method `{name}` should not take `self`, as declared in trait `{}`", tdef.name)
                };
                self.err(span, msg);
                continue;
            }
            if tfn.sig.params.len() != ifn.sig.params.len() {
                self.err(
                    span,
                    format!(
                        "method `{name}` has {} parameter(s), but trait `{}` declares {}",
                        ifn.sig.params.len(),
                        tdef.name,
                        tfn.sig.params.len()
                    ),
                );
                continue;
            }
            if tdef.is_op {
                continue;
            }
            let mut map = base.clone();
            if tfn.sig.generics.len() == ifn.sig.generics.len() {
                for (a, b) in tfn.sig.generics.iter().zip(&ifn.sig.generics) {
                    map.push((*a, Ty::Param(*b)));
                }
            }
            if tfn.sig.self_mode != ifn.sig.self_mode {
                self.err(span, format!("method `{name}` takes `self` differently from trait `{}`", tdef.name));
            }
            for (tp, ip) in tfn.sig.params.iter().zip(&ifn.sig.params) {
                let want = tp.ty.subst(&map);
                if want != ip.ty {
                    let (w, g) = (self.show(&want), self.show(&ip.ty));
                    self.err(
                        span,
                        format!("parameter `{}` of `{name}` has type `{g}`, but trait `{}` expects `{w}`", ip.name, tdef.name),
                    );
                } else if tp.mode != ip.mode {
                    self.err(span, format!("parameter `{}` of `{name}` has a different mode than in trait `{}`", ip.name, tdef.name));
                }
            }
            let want = tfn.sig.ret.subst(&map);
            if want != ifn.sig.ret {
                let (w, g) = (self.show(&want), self.show(&ifn.sig.ret));
                self.err(
                    span,
                    format!("method `{name}` returns `{g}`, but trait `{}` expects `{w}`", tdef.name),
                );
            }
        }
        for (name, ifid) in &imp.methods {
            if !tdef.methods.iter().any(|(n, _)| n == name) {
                let span = self.fn_decls[ifid.idx()].map(|d| d.name.span).unwrap_or(decl.self_ty.span);
                self.err(span, format!("method `{name}` is not a member of trait `{}`", tdef.name));
            }
        }
    }
}
