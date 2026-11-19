//! What the checker knows about the program's definitions (types, traits,
//! functions, impls). Purely data: no references into the AST.

use crate::ast::ParamMode;
use crate::types::*;

#[derive(Clone, Debug, PartialEq)]
pub struct TraitRef {
    pub trait_: TraitId,
    pub args: Vec<Ty>,
}

#[derive(Clone, Debug)]
pub struct GenericInfo {
    pub name: String,
    pub bounds: Vec<TraitRef>,
}

#[derive(Clone, Debug)]
pub struct FieldDef {
    pub name: String,
    pub ty: Ty,
    pub public: bool,
}

#[derive(Clone, Debug)]
pub struct VariantDef {
    pub name: String,
    pub fields: Vec<Ty>,
}

#[derive(Clone, Debug)]
pub enum AdtKind {
    Struct(Vec<FieldDef>),
    Enum(Vec<VariantDef>),
}

#[derive(Clone, Debug)]
pub struct AdtDef {
    pub name: String,
    pub generics: Vec<GenericId>,
    pub kind: AdtKind,
    pub module: usize,
    pub public: bool,
}

#[derive(Clone, Debug)]
pub struct ParamDef {
    pub name: String,
    pub mode: ParamMode,
    pub ty: Ty,
}

#[derive(Clone, Debug)]
pub struct FnSig {
    /// The function's own generic parameters (not including its impl's or trait's).
    pub generics: Vec<GenericId>,
    /// Mode of the `self` parameter, if the function takes one.
    pub self_mode: Option<ParamMode>,
    /// Parameters other than `self`.
    pub params: Vec<ParamDef>,
    pub ret: Ty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parent {
    Free,
    Impl(ImplId),
    Trait(TraitId),
}

#[derive(Clone, Debug)]
pub struct FnDef {
    pub name: String,
    pub sig: FnSig,
    pub parent: Parent,
    pub module: usize,
    pub public: bool,
    pub is_unsafe: bool,
    pub has_body: bool,
}

#[derive(Clone, Debug)]
pub struct TraitDef {
    pub name: String,
    /// The implicit `Self` parameter.
    pub self_generic: GenericId,
    pub generics: Vec<GenericId>,
    pub methods: Vec<(String, FnId)>,
    /// Operator traits (`Add`, `Mul`, ...): impls only need matching method names
    /// and arity, not identical signatures.
    pub is_op: bool,
    /// Traits whose satisfaction for primitive types is decided by rules in the
    /// checker (`Num`, `Eq`, `Display`, ...), in addition to explicit impls.
    pub builtin_rule: bool,
    pub module: usize,
    pub public: bool,
}

#[derive(Clone, Debug)]
pub struct ImplDef {
    pub generics: Vec<GenericId>,
    pub trait_ref: Option<TraitRef>,
    pub self_ty: Ty,
    pub methods: Vec<(String, FnId)>,
    pub module: usize,
}

#[derive(Clone, Debug)]
pub struct ConstDef {
    pub name: String,
    pub ty: Ty,
    pub module: usize,
    pub public: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Program {
    pub adts: Vec<AdtDef>,
    pub traits: Vec<TraitDef>,
    pub fns: Vec<FnDef>,
    pub impls: Vec<ImplDef>,
    pub consts: Vec<ConstDef>,
    pub generics: Vec<GenericInfo>,
}

impl Program {
    pub fn adt(&self, id: AdtId) -> &AdtDef {
        &self.adts[id.idx()]
    }
    pub fn trait_(&self, id: TraitId) -> &TraitDef {
        &self.traits[id.idx()]
    }
    pub fn fn_(&self, id: FnId) -> &FnDef {
        &self.fns[id.idx()]
    }
    pub fn impl_(&self, id: ImplId) -> &ImplDef {
        &self.impls[id.idx()]
    }
    pub fn generic(&self, id: GenericId) -> &GenericInfo {
        &self.generics[id.idx()]
    }

    pub fn new_generic(&mut self, name: &str) -> GenericId {
        self.generics.push(GenericInfo {
            name: name.to_string(),
            bounds: Vec::new(),
        });
        GenericId(self.generics.len() as u32 - 1)
    }

    pub fn find_adt(&self, name: &str) -> Option<AdtId> {
        self.adts
            .iter()
            .position(|a| a.name == name && a.module == 0)
            .map(|i| AdtId(i as u32))
    }

    pub fn find_trait(&self, name: &str) -> Option<TraitId> {
        self.traits
            .iter()
            .position(|t| t.name == name && t.module == 0)
            .map(|i| TraitId(i as u32))
    }

    /// Render a type for messages. `var_name` renders inference variables.
    pub fn show(&self, t: &Ty, var_name: &dyn Fn(TyVar) -> String) -> String {
        let s = |t: &Ty| self.show(t, var_name);
        let list = |ts: &[Ty]| ts.iter().map(|t| s(t)).collect::<Vec<_>>().join(", ");
        match t {
            Ty::Int(i) => i.name().to_string(),
            Ty::Float(f) => f.name().to_string(),
            Ty::Complex(FloatTy::F32) => "c64".to_string(),
            Ty::Complex(FloatTy::F64) => "c128".to_string(),
            Ty::Bool => "bool".to_string(),
            Ty::Char => "char".to_string(),
            Ty::Str => "str".to_string(),
            Ty::Never => "Never".to_string(),
            Ty::Tuple(ts) => {
                if ts.len() == 1 {
                    format!("({},)", s(&ts[0]))
                } else {
                    format!("({})", list(ts))
                }
            }
            Ty::Array(t, Some(n)) => format!("[{}; {n}]", s(t)),
            Ty::Array(t, None) => format!("[{}; _]", s(t)),
            Ty::Slice(t) => format!("[{}]", s(t)),
            Ty::Tensor(t, dims) => {
                let d: Vec<String> = dims
                    .iter()
                    .map(|d| match d {
                        Dim::Fixed(n) => n.to_string(),
                        Dim::Dyn => "_".to_string(),
                        Dim::Named(n) => n.clone(),
                    })
                    .collect();
                format!("Tensor<{}, [{}]>", s(t), d.join(", "))
            }
            Ty::Adt(id, args) => {
                let name = &self.adt(*id).name;
                if args.is_empty() {
                    name.clone()
                } else {
                    format!("{name}<{}>", list(args))
                }
            }
            Ty::Fn(ps, r) => {
                if r.is_unit() {
                    format!("fn({})", list(ps))
                } else {
                    format!("fn({}) -> {}", list(ps), s(r))
                }
            }
            Ty::Param(g) => self.generic(*g).name.clone(),
            Ty::Var(v) => var_name(*v),
            Ty::Error => "{error}".to_string(),
        }
    }
}
