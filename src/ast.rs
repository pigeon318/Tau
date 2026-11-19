//! The abstract syntax tree produced by the parser.
//!
//! Notes on a few deliberate choices:
//!  * Blocks do not mark a tail expression: the *last* `Stmt::Expr` in a block is
//!    the block's value (there are no semicolons to distinguish it).
//!  * `a.b(c)` is always `MethodCall`, and `a.b` is always `Field`; name resolution
//!    later decides whether `a` is a module, type, or value.
//!  * `a[i, j]` is `Index` with several indices. `vec[1, 2, 3]` therefore parses as
//!    `Index` on the name `vec`; resolution treats a `vec`/`tensor` builtin specially.

use crate::span::Span;

#[derive(Clone, Debug, PartialEq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    pub items: Vec<Item>,
}

// ---------------------------------------------------------------- items

#[derive(Clone, Debug, PartialEq)]
pub struct Attribute {
    pub name: Ident,
    pub args: Vec<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub attrs: Vec<Attribute>,
    pub public: bool,
    pub kind: ItemKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemKind {
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Trait(TraitDecl),
    Impl(ImplDecl),
    Use(UseTree),
    Const(ConstDecl),
    TypeAlias(TypeAliasDecl),
    /// `cluster lab = nodes ["a:1", "b:2"]` (post-v1; parsed so the syntax is reserved).
    Cluster(ClusterDecl),
}

#[derive(Clone, Debug, PartialEq)]
pub struct GenericParam {
    pub name: Ident,
    pub bounds: Vec<TypeExpr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamMode {
    /// Read-only borrow (the default).
    Borrow,
    /// Mutable borrow: `v: mut [f64]`, or `mut self`.
    Mut,
    /// Takes ownership: `own v: Vec<f64>`, or `own self`.
    Own,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: Ident,
    pub mode: ParamMode,
    /// `None` only for `self`.
    pub ty: Option<TypeExpr>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FnDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    /// `None` for signature-only declarations (in traits).
    pub body: Option<Block>,
    pub is_unsafe: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub public: bool,
    pub name: Ident,
    pub ty: TypeExpr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StructDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub name: Ident,
    pub fields: Vec<TypeExpr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnumDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<Variant>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TraitDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImplDecl {
    pub generics: Vec<GenericParam>,
    /// `Some(Add)` in `impl Add for Vec3`.
    pub trait_ref: Option<TypeExpr>,
    pub self_ty: TypeExpr,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UseKind {
    Simple { alias: Option<Ident> },
    Glob,
    Group(Vec<UseTree>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct UseTree {
    pub path: Vec<Ident>,
    pub kind: UseKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConstDecl {
    pub name: Ident,
    pub ty: TypeExpr,
    pub value: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeAliasDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub ty: TypeExpr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClusterDecl {
    pub name: Ident,
    pub nodes: Vec<Expr>,
}

// ---------------------------------------------------------------- types

#[derive(Clone, Debug, PartialEq)]
pub struct TypeExpr {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeKind {
    /// `f64`, `std.map.Map<K, V>`
    Path {
        segments: Vec<Ident>,
        args: Vec<TypeExpr>,
    },
    /// `(i32, f64)`; the unit type is the empty tuple.
    Tuple(Vec<TypeExpr>),
    /// `[f64; 3]`
    Array { elem: Box<TypeExpr>, len: Box<Expr> },
    /// `[f64]`
    Slice(Box<TypeExpr>),
    /// Tensor shape, as in `Tensor<f64, [3, _, N]>`.
    Dims(Vec<Dim>),
    /// `fn(f64) -> f64`
    Fn {
        params: Vec<TypeExpr>,
        ret: Option<Box<TypeExpr>>,
    },
    /// `_`
    Infer,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Dim {
    Fixed(u128),
    Dynamic,
    Named(Ident),
}

// ---------------------------------------------------------------- statements and blocks

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Let {
        pat: Pattern,
        ty: Option<TypeExpr>,
        init: Option<Expr>,
        span: Span,
    },
    Expr(Expr),
    Item(Item),
}

// ---------------------------------------------------------------- patterns

#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    pub kind: PatKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PatKind {
    Wild,
    /// A binding (or a unit variant / constant; resolution decides).
    Ident { name: String, mutable: bool },
    Lit(Box<Expr>),
    Range {
        lo: Box<Expr>,
        hi: Box<Expr>,
        inclusive: bool,
    },
    Tuple(Vec<Pattern>),
    /// `Circle(r)`, `Option.Some(x)`
    Variant {
        path: Vec<Ident>,
        args: Vec<Pattern>,
    },
    /// `Color.Red` (a dotted path with no arguments)
    Path(Vec<Ident>),
    Or(Vec<Pattern>),
}

// ---------------------------------------------------------------- expressions

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    MatMul,
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    And,
    Or,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        use BinOp::*;
        match self {
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Rem => "%",
            MatMul => "@",
            Pow => "^",
            BitAnd => "&",
            BitOr => "|",
            BitXor => "^^",
            Shl => "<<",
            Shr => ">>",
            And => "&&",
            Or => "||",
            Eq => "==",
            Ne => "!=",
            Lt => "<",
            Le => "<=",
            Gt => ">",
            Ge => ">=",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StrPiece {
    Lit(String),
    /// `{expr}` or `{expr:spec}`.
    Interp(Box<Expr>, Option<crate::fmtspec::Fmt>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchArm {
    pub pat: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClosureParam {
    pub pat: Pattern,
    pub ty: Option<TypeExpr>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Int { value: u128, suffix: Option<String> },
    Float { value: f64, suffix: Option<String> },
    Imag(f64),
    Bool(bool),
    Char(char),
    Str(Vec<StrPiece>),

    Ident(String),
    /// A name with explicit type arguments: `Map<String, usize>` in
    /// `Map<String, usize>.new()` or `alloc<u8>(n)`.
    TypeApp { name: Ident, args: Vec<TypeExpr> },

    Tuple(Vec<Expr>),
    Array(Vec<Expr>),
    /// `[value; len]`
    ArrayRepeat { value: Box<Expr>, len: Box<Expr> },

    Unary { op: UnOp, expr: Box<Expr> },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr> },
    /// `lhs = rhs` (`op: None`) or `lhs += rhs` (`op: Some(Add)`)
    Assign { op: Option<BinOp>, lhs: Box<Expr>, rhs: Box<Expr> },
    Cast { expr: Box<Expr>, ty: TypeExpr },

    Call { callee: Box<Expr>, args: Vec<Expr> },
    MethodCall { recv: Box<Expr>, name: Ident, args: Vec<Expr> },
    /// `a.b` or tuple field `a.0` (name is `"0"`)
    Field { base: Box<Expr>, name: Ident },
    Index { base: Box<Expr>, indices: Vec<Expr> },
    /// `expr?`
    Try(Box<Expr>),
    Range {
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
        inclusive: bool,
    },
    Closure {
        params: Vec<ClosureParam>,
        ret: Option<TypeExpr>,
        body: Box<Expr>,
    },

    Block(Block),
    Unsafe(Block),
    /// `else_` is either another `If` or a `Block`.
    If { cond: Box<Expr>, then: Block, else_: Option<Box<Expr>> },
    Match { scrutinee: Box<Expr>, arms: Vec<MatchArm> },
    Loop(Block),
    While { cond: Box<Expr>, body: Block },
    For {
        par: bool,
        pat: Pattern,
        iter: Box<Expr>,
        guard: Option<Box<Expr>>,
        body: Block,
    },
    Return(Option<Box<Expr>>),
    Break(Option<Box<Expr>>),
    Continue,

    /// Call-site mutable borrow: `scale(mut data, 2.0)`, `for x in mut v`.
    MutBorrow(Box<Expr>),
    /// `par sum(i in 0..n) a[i] * b[i]`
    ParReduce {
        op: Ident,
        pat: Pattern,
        iter: Box<Expr>,
        body: Box<Expr>,
    },
    Spawn(Block),
    /// `on lab { ... }` (post-v1)
    On { target: Box<Expr>, body: Block },
}
