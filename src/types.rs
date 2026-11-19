//! Semantic types.
//!
//! Tau's types contain no references: `mut` and `own` are *parameter modes*, tracked
//! separately (see `ast::ParamMode`), so `v: mut [f64]` has type `[f64]`.

macro_rules! id {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
        pub struct $name(pub u32);
        impl $name {
            pub fn idx(self) -> usize {
                self.0 as usize
            }
        }
    };
}

/// The most elements an array may have (it lives on the stack; use a `Vec` for more).
pub const MAX_ARRAY_LEN: u128 = 1 << 24;
/// The most elements a tensor may have (2 GB of `f64`).
pub const MAX_TENSOR_ELEMS: usize = 1 << 28;

id!(AdtId);
id!(TraitId);
id!(FnId);
id!(ImplId);
id!(ConstId);
id!(GenericId);
id!(TyVar);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    I128,
    Isize,
    U8,
    U16,
    U32,
    U64,
    U128,
    Usize,
}

impl IntTy {
    /// Width in bits.
    pub fn bits(self) -> u32 {
        use IntTy::*;
        match self {
            I8 | U8 => 8,
            I16 | U16 => 16,
            I32 | U32 => 32,
            I64 | U64 | Isize | Usize => 64,
            I128 | U128 => 128,
        }
    }

    pub fn is_signed(self) -> bool {
        use IntTy::*;
        matches!(self, I8 | I16 | I32 | I64 | I128 | Isize)
    }

    pub const ALL: [IntTy; 12] = [
        IntTy::I8,
        IntTy::I16,
        IntTy::I32,
        IntTy::I64,
        IntTy::I128,
        IntTy::Isize,
        IntTy::U8,
        IntTy::U16,
        IntTy::U32,
        IntTy::U64,
        IntTy::U128,
        IntTy::Usize,
    ];

    pub fn name(self) -> &'static str {
        use IntTy::*;
        match self {
            I8 => "i8",
            I16 => "i16",
            I32 => "i32",
            I64 => "i64",
            I128 => "i128",
            Isize => "isize",
            U8 => "u8",
            U16 => "u16",
            U32 => "u32",
            U64 => "u64",
            U128 => "u128",
            Usize => "usize",
        }
    }

    pub fn from_name(s: &str) -> Option<IntTy> {
        IntTy::ALL.iter().copied().find(|t| t.name() == s)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FloatTy {
    F32,
    F64,
}

impl FloatTy {
    pub fn name(self) -> &'static str {
        match self {
            FloatTy::F32 => "f32",
            FloatTy::F64 => "f64",
        }
    }
}

/// One dimension of a tensor shape.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Dim {
    Fixed(u128),
    /// `_`: known only at run time.
    Dyn,
    /// A symbolic dimension such as `N` (compile-time shape checking is post-v1).
    Named(String),
}

#[derive(Clone, PartialEq, Debug)]
pub enum Ty {
    Int(IntTy),
    Float(FloatTy),
    Complex(FloatTy),
    Bool,
    Char,
    Str,
    /// The type of expressions that never produce a value (`return`, `panic`).
    Never,
    /// The unit type is `Tuple([])`.
    Tuple(Vec<Ty>),
    /// `[T; N]`; the length is `None` when it is not a literal.
    Array(Box<Ty>, Option<u128>),
    Slice(Box<Ty>),
    Tensor(Box<Ty>, Vec<Dim>),
    Adt(AdtId, Vec<Ty>),
    Fn(Vec<Ty>, Box<Ty>),
    /// A generic type parameter (rigid inside its own item).
    Param(GenericId),
    /// An inference variable.
    Var(TyVar),
    /// Produced after an error was reported; unifies with everything.
    Error,
}

impl Ty {
    pub fn unit() -> Ty {
        Ty::Tuple(Vec::new())
    }

    pub fn is_unit(&self) -> bool {
        matches!(self, Ty::Tuple(v) if v.is_empty())
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Ty::Error)
    }

    pub fn usize() -> Ty {
        Ty::Int(IntTy::Usize)
    }

    pub fn f64() -> Ty {
        Ty::Float(FloatTy::F64)
    }

    /// Replace generic parameters according to `map`.
    pub fn subst(&self, map: &[(GenericId, Ty)]) -> Ty {
        if map.is_empty() {
            return self.clone();
        }
        self.map_children(&|t| match t {
            Ty::Param(g) => map.iter().find(|(k, _)| k == g).map(|(_, v)| v.clone()),
            _ => None,
        })
    }

    /// Rebuild the type, replacing any node for which `f` returns `Some`.
    pub fn map_children(&self, f: &dyn Fn(&Ty) -> Option<Ty>) -> Ty {
        if let Some(t) = f(self) {
            return t;
        }
        let m = |t: &Ty| t.map_children(f);
        match self {
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(m).collect()),
            Ty::Array(t, n) => Ty::Array(Box::new(m(t)), *n),
            Ty::Slice(t) => Ty::Slice(Box::new(m(t))),
            Ty::Tensor(t, d) => Ty::Tensor(Box::new(m(t)), d.clone()),
            Ty::Adt(id, ts) => Ty::Adt(*id, ts.iter().map(m).collect()),
            Ty::Fn(ps, r) => Ty::Fn(ps.iter().map(m).collect(), Box::new(m(r))),
            other => other.clone(),
        }
    }

    /// Does the type mention the inference variable `v`?
    pub fn contains_var(&self, v: TyVar) -> bool {
        match self {
            Ty::Var(w) => *w == v,
            Ty::Tuple(ts) | Ty::Adt(_, ts) => ts.iter().any(|t| t.contains_var(v)),
            Ty::Array(t, _) | Ty::Slice(t) | Ty::Tensor(t, _) => t.contains_var(v),
            Ty::Fn(ps, r) => ps.iter().any(|t| t.contains_var(v)) || r.contains_var(v),
            _ => false,
        }
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self, Ty::Int(_) | Ty::Float(_) | Ty::Complex(_))
    }
}
