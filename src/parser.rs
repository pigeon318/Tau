//! Recursive-descent parser with precedence climbing for expressions.
//!
//! Operator precedence, loosest to tightest:
//!
//! | level | operators                                  |
//! |-------|--------------------------------------------|
//! | 0     | `=` `+=` `-=` ... (right associative)       |
//! | 1     | `..` `..=`                                  |
//! | 2     | `\|\|`                                      |
//! | 3     | `&&`                                        |
//! | 4     | `==` `!=` `<` `>` `<=` `>=`                 |
//! | 5     | `\|`                                        |
//! | 6     | `^^`                                        |
//! | 7     | `&`                                         |
//! | 8     | `<<` `>>`                                   |
//! | 9     | `+` `-`                                     |
//! | 10    | `*` `/` `%` `@`                             |
//! | 11    | `as` (cast)                                 |
//! | 12    | unary `-` `!` `~` `mut`                     |
//! | 13    | `^` power (right associative)               |
//! | 14    | postfix: call, index, `.field`, `?`          |
//!
//! Power binds tighter than unary minus, as in maths: `-x^2` is `-(x^2)`.

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::lexer::{lex, lex_at};
use crate::span::Span;
use crate::token::{StrPart, Token, TokenKind as K};

type PResult<T> = Result<T, Diagnostic>;

/// Parse a whole source file.
pub fn parse_module(src: &str) -> PResult<Module> {
    let toks = lex(src)?;
    Parser::new(toks).module()
}

/// Parse a single expression (handy for tests and tooling).
pub fn parse_expr(src: &str) -> PResult<Expr> {
    let toks = lex(src)?;
    let mut p = Parser::new(toks);
    p.skip_newlines();
    let e = p.expr()?;
    p.skip_newlines();
    if !p.check(&K::Eof) {
        return Err(p.err_expected("end of input"));
    }
    Ok(e)
}

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(toks: Vec<Token>) -> Parser {
        assert!(
            matches!(toks.last(), Some(t) if t.kind == K::Eof),
            "token stream must end with Eof"
        );
        Parser { toks, pos: 0 }
    }

    // ------------------------------------------------------------ token helpers

    fn kind(&self) -> &K {
        &self.toks[self.pos].kind
    }

    fn kind_at(&self, n: usize) -> &K {
        let i = (self.pos + n).min(self.toks.len() - 1);
        &self.toks[i].kind
    }

    fn span(&self) -> Span {
        self.toks[self.pos].span
    }

    fn prev_end(&self) -> usize {
        if self.pos == 0 {
            0
        } else {
            self.toks[self.pos - 1].span.end
        }
    }

    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn check(&self, k: &K) -> bool {
        self.kind() == k
    }

    fn eat(&mut self, k: &K) -> bool {
        if self.check(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, k: &K, what: &str) -> PResult<Token> {
        if self.check(k) {
            Ok(self.bump())
        } else {
            Err(self.err_expected(what))
        }
    }

    fn skip_newlines(&mut self) {
        while *self.kind() == K::Newline {
            self.pos += 1;
        }
    }

    fn err_expected(&self, what: &str) -> Diagnostic {
        if *self.kind() == K::Semi {
            return self.semicolon_error();
        }
        Diagnostic::new(
            format!("expected {what}, found {}", self.kind().describe()),
            self.span(),
        )
    }

    fn semicolon_error(&self) -> Diagnostic {
        Diagnostic::new("Tau doesn't use semicolons", self.span())
            .with_hint("remove the `;` — a new line ends a statement")
    }

    fn ident(&mut self, what: &str) -> PResult<Ident> {
        if let K::Ident(name) = self.kind().clone() {
            let span = self.bump().span;
            Ok(Ident { name, span })
        } else {
            Err(self.err_expected(what))
        }
    }

    fn is_ident(&self, name: &str) -> bool {
        matches!(self.kind(), K::Ident(s) if s == name)
    }

    /// After an element of a `,`-or-newline separated list: consume the
    /// separator, or accept the closing token.
    fn list_sep(&mut self, close: &K) -> PResult<()> {
        let mut saw = self.eat(&K::Comma);
        if *self.kind() == K::Newline {
            self.skip_newlines();
            saw = true;
        }
        if saw || self.check(close) {
            Ok(())
        } else {
            Err(self.err_expected(&format!("`,`, a new line or {}", close.describe())))
        }
    }

    // ------------------------------------------------------------ module and items

    fn module(&mut self) -> PResult<Module> {
        let mut items = Vec::new();
        self.skip_newlines();
        while !self.check(&K::Eof) {
            items.push(self.item()?);
            match self.kind() {
                K::Newline => self.skip_newlines(),
                K::Eof => {}
                _ => return Err(self.err_expected("a new line after this item")),
            }
        }
        Ok(Module { items })
    }

    fn attribute(&mut self) -> PResult<Attribute> {
        let start = self.span().start;
        self.expect(&K::Hash, "`#`")?;
        self.expect(&K::LBracket, "`[`")?;
        let name = self.ident("an attribute name")?;
        let mut args = Vec::new();
        if self.eat(&K::LParen) {
            while !self.check(&K::RParen) {
                args.push(self.expr()?);
                if !self.eat(&K::Comma) {
                    break;
                }
            }
            self.expect(&K::RParen, "`)`")?;
        }
        self.expect(&K::RBracket, "`]`")?;
        Ok(Attribute {
            name,
            args,
            span: Span::new(start, self.prev_end()),
        })
    }

    fn starts_item(&self) -> bool {
        match self.kind() {
            K::Pub | K::Hash | K::Fn | K::Struct | K::Enum | K::Trait | K::Impl | K::Use
            | K::Const | K::Type | K::Cluster => true,
            K::Unsafe => *self.kind_at(1) == K::Fn,
            _ => false,
        }
    }

    fn item(&mut self) -> PResult<Item> {
        let start = self.span().start;
        let mut attrs = Vec::new();
        while self.check(&K::Hash) {
            attrs.push(self.attribute()?);
            self.skip_newlines();
        }
        let public = self.eat(&K::Pub);
        let kind = match self.kind().clone() {
            K::Fn => ItemKind::Fn(self.fn_decl(false)?),
            K::Unsafe if *self.kind_at(1) == K::Fn => {
                self.bump();
                ItemKind::Fn(self.fn_decl(true)?)
            }
            K::Struct => ItemKind::Struct(self.struct_decl()?),
            K::Enum => ItemKind::Enum(self.enum_decl()?),
            K::Trait => ItemKind::Trait(self.trait_decl()?),
            K::Impl => ItemKind::Impl(self.impl_decl()?),
            K::Use => {
                self.bump();
                ItemKind::Use(self.use_tree()?)
            }
            K::Const => ItemKind::Const(self.const_decl()?),
            K::Type => ItemKind::TypeAlias(self.type_alias()?),
            K::Cluster => ItemKind::Cluster(self.cluster_decl()?),
            _ => {
                return Err(self.err_expected(
                    "an item (`fn`, `struct`, `enum`, `trait`, `impl`, `use`, `const` or `type`)",
                ))
            }
        };
        Ok(Item {
            attrs,
            public,
            kind,
            span: Span::new(start, self.prev_end()),
        })
    }

    fn generics(&mut self) -> PResult<Vec<GenericParam>> {
        let mut out = Vec::new();
        if !self.eat(&K::Lt) {
            return Ok(out);
        }
        while !self.check(&K::Gt) {
            let name = self.ident("a type parameter name")?;
            let mut bounds = Vec::new();
            if self.eat(&K::Colon) {
                loop {
                    bounds.push(self.ty()?);
                    if !self.eat(&K::Plus) {
                        break;
                    }
                }
            }
            out.push(GenericParam { name, bounds });
            if !self.eat(&K::Comma) {
                break;
            }
        }
        self.expect(&K::Gt, "`>`")?;
        Ok(out)
    }

    fn fn_decl(&mut self, is_unsafe: bool) -> PResult<FnDecl> {
        self.expect(&K::Fn, "`fn`")?;
        let name = self.ident("a function name")?;
        let generics = self.generics()?;
        self.expect(&K::LParen, "`(`")?;
        let mut params = Vec::new();
        while !self.check(&K::RParen) {
            params.push(self.param()?);
            if !self.eat(&K::Comma) {
                break;
            }
        }
        self.expect(&K::RParen, "`)`")?;
        let ret = if self.eat(&K::Arrow) {
            Some(self.ty()?)
        } else {
            None
        };
        let body = if self.check(&K::LBrace) {
            Some(self.block()?)
        } else {
            None
        };
        Ok(FnDecl {
            name,
            generics,
            params,
            ret,
            body,
            is_unsafe,
        })
    }

    fn param(&mut self) -> PResult<Param> {
        let start = self.span().start;
        let mut mode = ParamMode::Borrow;
        if self.eat(&K::Own) {
            mode = ParamMode::Own;
        } else if self.check(&K::Mut) {
            if matches!(self.kind_at(1), K::Ident(s) if s == "self") {
                self.bump();
                mode = ParamMode::Mut;
            } else {
                return Err(Diagnostic::new(
                    "a mutable borrow is written on the type, not the name",
                    self.span(),
                )
                .with_hint("write `name: mut Type`"));
            }
        }
        let name = self.ident("a parameter name")?;
        if name.name == "self" {
            return Ok(Param {
                span: Span::new(start, self.prev_end()),
                name,
                mode,
                ty: None,
            });
        }
        self.expect(&K::Colon, "`:` followed by a type")?;
        if self.check(&K::Mut) {
            if mode == ParamMode::Own {
                return Err(Diagnostic::new(
                    "a parameter can't be both `own` and `mut`",
                    self.span(),
                ));
            }
            self.bump();
            mode = ParamMode::Mut;
        }
        let ty = self.ty()?;
        Ok(Param {
            span: Span::new(start, self.prev_end()),
            name,
            mode,
            ty: Some(ty),
        })
    }

    fn struct_decl(&mut self) -> PResult<StructDecl> {
        self.expect(&K::Struct, "`struct`")?;
        let name = self.ident("a struct name")?;
        let generics = self.generics()?;
        self.expect(&K::LBrace, "`{`")?;
        self.skip_newlines();
        let mut fields = Vec::new();
        while !self.check(&K::RBrace) {
            let public = self.eat(&K::Pub);
            let fname = self.ident("a field name")?;
            self.expect(&K::Colon, "`:` followed by the field's type")?;
            let ty = self.ty()?;
            fields.push(Field {
                public,
                name: fname,
                ty,
            });
            self.list_sep(&K::RBrace)?;
        }
        self.expect(&K::RBrace, "`}`")?;
        Ok(StructDecl {
            name,
            generics,
            fields,
        })
    }

    fn enum_decl(&mut self) -> PResult<EnumDecl> {
        self.expect(&K::Enum, "`enum`")?;
        let name = self.ident("an enum name")?;
        let generics = self.generics()?;
        self.expect(&K::LBrace, "`{`")?;
        self.skip_newlines();
        let mut variants = Vec::new();
        while !self.check(&K::RBrace) {
            let vname = self.ident("a variant name")?;
            let mut fields = Vec::new();
            if self.eat(&K::LParen) {
                while !self.check(&K::RParen) {
                    fields.push(self.ty()?);
                    if !self.eat(&K::Comma) {
                        break;
                    }
                }
                self.expect(&K::RParen, "`)`")?;
            }
            variants.push(Variant {
                name: vname,
                fields,
            });
            self.list_sep(&K::RBrace)?;
        }
        self.expect(&K::RBrace, "`}`")?;
        Ok(EnumDecl {
            name,
            generics,
            variants,
        })
    }

    /// `{ item* }` for trait and impl bodies.
    fn item_body(&mut self) -> PResult<Vec<Item>> {
        self.expect(&K::LBrace, "`{`")?;
        self.skip_newlines();
        let mut items = Vec::new();
        while !self.check(&K::RBrace) {
            items.push(self.item()?);
            match self.kind() {
                K::Newline => self.skip_newlines(),
                K::RBrace => {}
                _ => return Err(self.err_expected("a new line or `}` after this item")),
            }
        }
        self.expect(&K::RBrace, "`}`")?;
        Ok(items)
    }

    fn trait_decl(&mut self) -> PResult<TraitDecl> {
        self.expect(&K::Trait, "`trait`")?;
        let name = self.ident("a trait name")?;
        let generics = self.generics()?;
        let items = self.item_body()?;
        Ok(TraitDecl {
            name,
            generics,
            items,
        })
    }

    fn impl_decl(&mut self) -> PResult<ImplDecl> {
        self.expect(&K::Impl, "`impl`")?;
        let generics = self.generics()?;
        let first = self.ty()?;
        let (trait_ref, self_ty) = if self.eat(&K::For) {
            (Some(first), self.ty()?)
        } else {
            (None, first)
        };
        let items = self.item_body()?;
        Ok(ImplDecl {
            generics,
            trait_ref,
            self_ty,
            items,
        })
    }

    fn use_tree(&mut self) -> PResult<UseTree> {
        let start = self.span().start;
        let mut path = vec![self.ident("a module name")?];
        let mut kind = UseKind::Simple { alias: None };
        while self.eat(&K::Dot) {
            match self.kind() {
                K::LBrace => {
                    self.bump();
                    self.skip_newlines();
                    let mut trees = Vec::new();
                    while !self.check(&K::RBrace) {
                        trees.push(self.use_tree()?);
                        self.list_sep(&K::RBrace)?;
                    }
                    self.expect(&K::RBrace, "`}`")?;
                    kind = UseKind::Group(trees);
                    break;
                }
                K::Star => {
                    self.bump();
                    kind = UseKind::Glob;
                    break;
                }
                _ => path.push(self.ident("a name")?),
            }
        }
        if matches!(kind, UseKind::Simple { .. }) && self.eat(&K::As) {
            let alias = self.ident("an alias name")?;
            kind = UseKind::Simple { alias: Some(alias) };
        }
        Ok(UseTree {
            path,
            kind,
            span: Span::new(start, self.prev_end()),
        })
    }

    fn const_decl(&mut self) -> PResult<ConstDecl> {
        self.expect(&K::Const, "`const`")?;
        let name = self.ident("a constant name")?;
        self.expect(&K::Colon, "`:` followed by the constant's type")?;
        let ty = self.ty()?;
        self.expect(&K::Eq, "`=`")?;
        let value = self.expr()?;
        Ok(ConstDecl { name, ty, value })
    }

    fn type_alias(&mut self) -> PResult<TypeAliasDecl> {
        self.expect(&K::Type, "`type`")?;
        let name = self.ident("a type name")?;
        let generics = self.generics()?;
        self.expect(&K::Eq, "`=`")?;
        let ty = self.ty()?;
        Ok(TypeAliasDecl { name, generics, ty })
    }

    fn cluster_decl(&mut self) -> PResult<ClusterDecl> {
        self.expect(&K::Cluster, "`cluster`")?;
        let name = self.ident("a cluster name")?;
        self.expect(&K::Eq, "`=`")?;
        if !self.is_ident("nodes") {
            return Err(self.err_expected("`nodes [...]`"));
        }
        self.bump();
        self.expect(&K::LBracket, "`[` followed by a list of node addresses")?;
        let mut nodes = Vec::new();
        while !self.check(&K::RBracket) {
            nodes.push(self.expr()?);
            if !self.eat(&K::Comma) {
                break;
            }
        }
        self.expect(&K::RBracket, "`]`")?;
        Ok(ClusterDecl { name, nodes })
    }

    // ------------------------------------------------------------ types

    fn ty(&mut self) -> PResult<TypeExpr> {
        self.ty_inner(true)
    }

    /// `allow_args` is false for cast targets (`x as usize < y` must not read `<` as generics).
    fn ty_inner(&mut self, allow_args: bool) -> PResult<TypeExpr> {
        let start = self.span().start;
        let kind = match self.kind().clone() {
            K::Underscore => {
                self.bump();
                TypeKind::Infer
            }
            K::LParen => {
                self.bump();
                let mut elems = Vec::new();
                let mut trailing_comma = false;
                while !self.check(&K::RParen) {
                    elems.push(self.ty()?);
                    trailing_comma = self.eat(&K::Comma);
                    if !trailing_comma {
                        break;
                    }
                }
                self.expect(&K::RParen, "`)`")?;
                if elems.len() == 1 && !trailing_comma {
                    return Ok(elems.pop().unwrap());
                }
                TypeKind::Tuple(elems)
            }
            K::LBracket => {
                self.bump();
                self.bracket_type()?
            }
            K::Fn => {
                self.bump();
                self.expect(&K::LParen, "`(`")?;
                let mut params = Vec::new();
                while !self.check(&K::RParen) {
                    params.push(self.ty()?);
                    if !self.eat(&K::Comma) {
                        break;
                    }
                }
                self.expect(&K::RParen, "`)`")?;
                let ret = if self.eat(&K::Arrow) {
                    Some(Box::new(self.ty()?))
                } else {
                    None
                };
                TypeKind::Fn { params, ret }
            }
            K::Ident(_) => {
                let mut segments = vec![self.ident("a type name")?];
                while self.check(&K::Dot) && matches!(self.kind_at(1), K::Ident(_)) {
                    self.bump();
                    segments.push(self.ident("a type name")?);
                }
                let mut args = Vec::new();
                if allow_args && self.eat(&K::Lt) {
                    while !self.check(&K::Gt) {
                        args.push(self.ty()?);
                        if !self.eat(&K::Comma) {
                            break;
                        }
                    }
                    self.expect(&K::Gt, "`>`")?;
                }
                TypeKind::Path { segments, args }
            }
            _ => return Err(self.err_expected("a type")),
        };
        Ok(TypeExpr {
            kind,
            span: Span::new(start, self.prev_end()),
        })
    }

    /// After `[` in a type: slice `[T]`, array `[T; n]`, or tensor shape `[3, _, N]`.
    fn bracket_type(&mut self) -> PResult<TypeKind> {
        if matches!(self.kind(), K::Int { .. } | K::Underscore) {
            return self.dims_rest(Vec::new());
        }
        let t = self.ty()?;
        if self.eat(&K::Semi) {
            let len = self.expr()?;
            self.expect(&K::RBracket, "`]`")?;
            return Ok(TypeKind::Array {
                elem: Box::new(t),
                len: Box::new(len),
            });
        }
        if self.check(&K::Comma) {
            // `[N, M]`: a shape whose first dimension was parsed as a type name
            let first = match &t.kind {
                TypeKind::Path { segments, args } if segments.len() == 1 && args.is_empty() => {
                    Dim::Named(segments[0].clone())
                }
                _ => return Err(self.err_expected("`;` or `]`")),
            };
            self.bump();
            return self.dims_rest(vec![first]);
        }
        self.expect(&K::RBracket, "`]`")?;
        Ok(TypeKind::Slice(Box::new(t)))
    }

    fn dims_rest(&mut self, mut dims: Vec<Dim>) -> PResult<TypeKind> {
        while !self.check(&K::RBracket) {
            let d = match self.kind().clone() {
                K::Int { value, .. } => {
                    self.bump();
                    Dim::Fixed(value)
                }
                K::Underscore => {
                    self.bump();
                    Dim::Dynamic
                }
                K::Ident(_) => Dim::Named(self.ident("a dimension")?),
                _ => return Err(self.err_expected("a dimension (a number, `_` or a name)")),
            };
            dims.push(d);
            if !self.eat(&K::Comma) {
                break;
            }
        }
        self.expect(&K::RBracket, "`]`")?;
        Ok(TypeKind::Dims(dims))
    }

    // ------------------------------------------------------------ patterns

    fn pattern(&mut self) -> PResult<Pattern> {
        let start = self.span().start;
        let first = self.pattern_primary()?;
        if !self.check(&K::Pipe) {
            return Ok(first);
        }
        let mut alts = vec![first];
        while self.eat(&K::Pipe) {
            alts.push(self.pattern_primary()?);
        }
        Ok(Pattern {
            kind: PatKind::Or(alts),
            span: Span::new(start, self.prev_end()),
        })
    }

    fn pattern_primary(&mut self) -> PResult<Pattern> {
        let start = self.span().start;
        let kind = match self.kind().clone() {
            K::Underscore => {
                self.bump();
                PatKind::Wild
            }
            K::Mut => {
                self.bump();
                let name = self.ident("a variable name after `mut`")?;
                PatKind::Ident {
                    name: name.name,
                    mutable: true,
                }
            }
            K::Int { .. }
            | K::Float { .. }
            | K::Str(_)
            | K::Char(_)
            | K::True
            | K::False
            | K::Minus => {
                let lo = if self.check(&K::Minus) {
                    let s = self.bump().span;
                    let operand = self.literal()?;
                    Expr {
                        span: s.to(operand.span),
                        kind: ExprKind::Unary {
                            op: UnOp::Neg,
                            expr: Box::new(operand),
                        },
                    }
                } else {
                    self.literal()?
                };
                if matches!(self.kind(), K::DotDot | K::DotDotEq) {
                    let inclusive = self.check(&K::DotDotEq);
                    self.bump();
                    let hi = if self.check(&K::Minus) {
                        let s = self.bump().span;
                        let operand = self.literal()?;
                        Expr {
                            span: s.to(operand.span),
                            kind: ExprKind::Unary {
                                op: UnOp::Neg,
                                expr: Box::new(operand),
                            },
                        }
                    } else {
                        self.literal()?
                    };
                    PatKind::Range {
                        lo: Box::new(lo),
                        hi: Box::new(hi),
                        inclusive,
                    }
                } else {
                    PatKind::Lit(Box::new(lo))
                }
            }
            K::LParen => {
                self.bump();
                let mut elems = Vec::new();
                let mut trailing_comma = false;
                while !self.check(&K::RParen) {
                    elems.push(self.pattern()?);
                    trailing_comma = self.eat(&K::Comma);
                    if !trailing_comma {
                        break;
                    }
                }
                self.expect(&K::RParen, "`)`")?;
                if elems.len() == 1 && !trailing_comma {
                    return Ok(elems.pop().unwrap());
                }
                PatKind::Tuple(elems)
            }
            K::Ident(_) => {
                let mut path = vec![self.ident("a name")?];
                while self.check(&K::Dot) && matches!(self.kind_at(1), K::Ident(_)) {
                    self.bump();
                    path.push(self.ident("a name")?);
                }
                if self.eat(&K::LParen) {
                    let mut args = Vec::new();
                    while !self.check(&K::RParen) {
                        args.push(self.pattern()?);
                        if !self.eat(&K::Comma) {
                            break;
                        }
                    }
                    self.expect(&K::RParen, "`)`")?;
                    PatKind::Variant { path, args }
                } else if path.len() == 1 {
                    PatKind::Ident {
                        name: path.pop().unwrap().name,
                        mutable: false,
                    }
                } else {
                    PatKind::Path(path)
                }
            }
            _ => return Err(self.err_expected("a pattern")),
        };
        Ok(Pattern {
            kind,
            span: Span::new(start, self.prev_end()),
        })
    }

    // ------------------------------------------------------------ blocks and statements

    fn block(&mut self) -> PResult<Block> {
        let start = self.span().start;
        self.expect(&K::LBrace, "`{`")?;
        self.skip_newlines();
        let mut stmts = Vec::new();
        while !self.check(&K::RBrace) {
            stmts.push(self.stmt()?);
            match self.kind() {
                K::Newline => self.skip_newlines(),
                K::RBrace => {}
                _ => return Err(self.err_expected("a new line or `}` after this statement")),
            }
        }
        self.expect(&K::RBrace, "`}`")?;
        Ok(Block {
            stmts,
            span: Span::new(start, self.prev_end()),
        })
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        if self.check(&K::Let) {
            let start = self.bump().span.start;
            let pat = self.pattern()?;
            let ty = if self.eat(&K::Colon) {
                Some(self.ty()?)
            } else {
                None
            };
            let init = if self.eat(&K::Eq) {
                Some(self.expr()?)
            } else {
                None
            };
            return Ok(Stmt::Let {
                pat,
                ty,
                init,
                span: Span::new(start, self.prev_end()),
            });
        }
        if self.starts_item() {
            return Ok(Stmt::Item(self.item()?));
        }
        Ok(Stmt::Expr(self.expr()?))
    }

    // ------------------------------------------------------------ expressions

    pub fn expr(&mut self) -> PResult<Expr> {
        self.assign()
    }

    fn assign_op(&self) -> Option<Option<BinOp>> {
        Some(match self.kind() {
            K::Eq => None,
            K::PlusEq => Some(BinOp::Add),
            K::MinusEq => Some(BinOp::Sub),
            K::StarEq => Some(BinOp::Mul),
            K::SlashEq => Some(BinOp::Div),
            K::PercentEq => Some(BinOp::Rem),
            K::AtEq => Some(BinOp::MatMul),
            K::CaretEq => Some(BinOp::Pow),
            K::AmpEq => Some(BinOp::BitAnd),
            K::PipeEq => Some(BinOp::BitOr),
            _ => return None,
        })
    }

    fn assign(&mut self) -> PResult<Expr> {
        let lhs = self.range()?;
        if let Some(op) = self.assign_op() {
            self.bump();
            let rhs = self.assign()?;
            return Ok(Expr {
                span: lhs.span.to(rhs.span),
                kind: ExprKind::Assign {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            });
        }
        Ok(lhs)
    }

    fn can_start_expr(k: &K) -> bool {
        matches!(
            k,
            K::Ident(_)
                | K::Int { .. }
                | K::Float { .. }
                | K::Imag { .. }
                | K::Str(_)
                | K::Char(_)
                | K::True
                | K::False
                | K::LParen
                | K::LBracket
                | K::LBrace
                | K::Minus
                | K::Bang
                | K::Tilde
                | K::Mut
                | K::Pipe
                | K::PipePipe
                | K::If
                | K::Match
                | K::For
                | K::While
                | K::Loop
                | K::Unsafe
                | K::Spawn
                | K::On
                | K::Par
                | K::Return
                | K::Break
                | K::Continue
        )
    }

    fn range(&mut self) -> PResult<Expr> {
        let start = self.span().start;
        if matches!(self.kind(), K::DotDot | K::DotDotEq) {
            let inclusive = self.check(&K::DotDotEq);
            self.bump();
            let end = self.range_end()?;
            return Ok(Expr {
                span: Span::new(start, self.prev_end()),
                kind: ExprKind::Range {
                    start: None,
                    end,
                    inclusive,
                },
            });
        }
        let lhs = self.binary(0)?;
        if matches!(self.kind(), K::DotDot | K::DotDotEq) {
            let inclusive = self.check(&K::DotDotEq);
            self.bump();
            let end = self.range_end()?;
            return Ok(Expr {
                span: Span::new(start, self.prev_end()),
                kind: ExprKind::Range {
                    start: Some(Box::new(lhs)),
                    end,
                    inclusive,
                },
            });
        }
        Ok(lhs)
    }

    fn range_end(&mut self) -> PResult<Option<Box<Expr>>> {
        if Self::can_start_expr(self.kind()) && !self.check(&K::LBrace) {
            Ok(Some(Box::new(self.binary(0)?)))
        } else {
            Ok(None)
        }
    }

    /// The binary operator at the cursor: (op, precedence, tokens consumed).
    fn binary_op(&self) -> Option<(BinOp, u8, usize)> {
        Some(match self.kind() {
            K::PipePipe => (BinOp::Or, 0, 1),
            K::AmpAmp => (BinOp::And, 1, 1),
            K::EqEq => (BinOp::Eq, 2, 1),
            K::BangEq => (BinOp::Ne, 2, 1),
            K::Lt => (BinOp::Lt, 2, 1),
            K::Le => (BinOp::Le, 2, 1),
            K::Ge => (BinOp::Ge, 2, 1),
            K::Gt => {
                // `>>` is two adjacent `Gt` tokens
                let adjacent = *self.kind_at(1) == K::Gt
                    && self.toks[self.pos].span.end == self.toks[self.pos + 1].span.start;
                if adjacent {
                    (BinOp::Shr, 6, 2)
                } else {
                    (BinOp::Gt, 2, 1)
                }
            }
            K::Pipe => (BinOp::BitOr, 3, 1),
            K::CaretCaret => (BinOp::BitXor, 4, 1),
            K::Amp => (BinOp::BitAnd, 5, 1),
            K::Shl => (BinOp::Shl, 6, 1),
            K::Plus => (BinOp::Add, 7, 1),
            K::Minus => (BinOp::Sub, 7, 1),
            K::Star => (BinOp::Mul, 8, 1),
            K::Slash => (BinOp::Div, 8, 1),
            K::Percent => (BinOp::Rem, 8, 1),
            K::At => (BinOp::MatMul, 8, 1),
            _ => return None,
        })
    }

    fn binary(&mut self, min_prec: u8) -> PResult<Expr> {
        let mut lhs = self.cast()?;
        while let Some((op, prec, ntok)) = self.binary_op() {
            if prec < min_prec {
                break;
            }
            for _ in 0..ntok {
                self.bump();
            }
            let rhs = self.binary(prec + 1)?;
            lhs = Expr {
                span: lhs.span.to(rhs.span),
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    fn cast(&mut self) -> PResult<Expr> {
        let mut e = self.unary()?;
        while self.eat(&K::As) {
            let ty = self.ty_inner(false)?;
            e = Expr {
                span: Span::new(e.span.start, self.prev_end()),
                kind: ExprKind::Cast {
                    expr: Box::new(e),
                    ty,
                },
            };
        }
        Ok(e)
    }

    fn unary(&mut self) -> PResult<Expr> {
        let start = self.span().start;
        let op = match self.kind() {
            K::Minus => Some(Some(UnOp::Neg)),
            K::Bang => Some(Some(UnOp::Not)),
            K::Tilde => Some(Some(UnOp::BitNot)),
            K::Mut => Some(None),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let operand = self.unary()?;
            let span = Span::new(start, operand.span.end);
            return Ok(Expr {
                span,
                kind: match op {
                    Some(op) => ExprKind::Unary {
                        op,
                        expr: Box::new(operand),
                    },
                    None => ExprKind::MutBorrow(Box::new(operand)),
                },
            });
        }
        self.power()
    }

    fn power(&mut self) -> PResult<Expr> {
        let base = self.postfix()?;
        if self.eat(&K::Caret) {
            let exp = self.unary()?; // right associative, allows `2^-1`
            return Ok(Expr {
                span: base.span.to(exp.span),
                kind: ExprKind::Binary {
                    op: BinOp::Pow,
                    lhs: Box::new(base),
                    rhs: Box::new(exp),
                },
            });
        }
        Ok(base)
    }

    fn args(&mut self) -> PResult<Vec<Expr>> {
        let mut args = Vec::new();
        while !self.check(&K::RParen) {
            args.push(self.expr()?);
            if !self.eat(&K::Comma) {
                break;
            }
        }
        self.expect(&K::RParen, "`)`")?;
        Ok(args)
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            let start = e.span.start;
            match self.kind() {
                K::LParen => {
                    self.bump();
                    let args = self.args()?;
                    e = Expr {
                        span: Span::new(start, self.prev_end()),
                        kind: ExprKind::Call {
                            callee: Box::new(e),
                            args,
                        },
                    };
                }
                K::LBracket => {
                    self.bump();
                    let mut indices = Vec::new();
                    while !self.check(&K::RBracket) {
                        indices.push(self.expr()?);
                        if !self.eat(&K::Comma) {
                            break;
                        }
                    }
                    self.expect(&K::RBracket, "`]`")?;
                    e = Expr {
                        span: Span::new(start, self.prev_end()),
                        kind: ExprKind::Index {
                            base: Box::new(e),
                            indices,
                        },
                    };
                }
                K::Dot => {
                    self.bump();
                    let name = match self.kind().clone() {
                        K::Ident(_) => self.ident("a field or method name")?,
                        K::Int { value, suffix: None } => {
                            let span = self.bump().span;
                            Ident {
                                name: value.to_string(),
                                span,
                            }
                        }
                        _ => return Err(self.err_expected("a field or method name after `.`")),
                    };
                    if self.eat(&K::LParen) {
                        let args = self.args()?;
                        e = Expr {
                            span: Span::new(start, self.prev_end()),
                            kind: ExprKind::MethodCall {
                                recv: Box::new(e),
                                name,
                                args,
                            },
                        };
                    } else {
                        e = Expr {
                            span: Span::new(start, self.prev_end()),
                            kind: ExprKind::Field {
                                base: Box::new(e),
                                name,
                            },
                        };
                    }
                }
                K::Question => {
                    self.bump();
                    e = Expr {
                        span: Span::new(start, self.prev_end()),
                        kind: ExprKind::Try(Box::new(e)),
                    };
                }
                _ => break,
            }
        }
        Ok(e)
    }

    /// A literal token as an expression; string interpolations are parsed here.
    fn literal(&mut self) -> PResult<Expr> {
        let tok = self.bump();
        let span = tok.span;
        let kind = match tok.kind {
            K::Int { value, suffix } => ExprKind::Int { value, suffix },
            K::Float { value, suffix } => ExprKind::Float { value, suffix },
            K::Imag { value } => ExprKind::Imag(value),
            K::Char(c) => ExprKind::Char(c),
            K::True => ExprKind::Bool(true),
            K::False => ExprKind::Bool(false),
            K::Str(parts) => {
                let mut pieces = Vec::new();
                for part in parts {
                    match part {
                        StrPart::Lit(s) => pieces.push(StrPiece::Lit(s)),
                        StrPart::Interp { src, offset } => {
                            let toks = lex_at(&src, offset)?;
                            let mut sub = Parser::new(toks);
                            sub.skip_newlines();
                            let e = sub.expr()?;
                            sub.skip_newlines();
                            if !sub.check(&K::Eof) {
                                return Err(sub.err_expected("the end of the `{...}` expression"));
                            }
                            pieces.push(StrPiece::Interp(Box::new(e)));
                        }
                    }
                }
                ExprKind::Str(pieces)
            }
            other => {
                return Err(Diagnostic::new(
                    format!("expected a literal, found {}", other.describe()),
                    span,
                ))
            }
        };
        Ok(Expr { kind, span })
    }

    /// Try to read `<T, U>` right after a name, as in `Map<String, usize>.new()`.
    /// Succeeds only if the arguments parse as types and the closing `>` is followed
    /// by `.` or `(`; otherwise the cursor is restored and `<` is a comparison.
    fn try_type_args(&mut self) -> Option<Vec<TypeExpr>> {
        let save = self.pos;
        self.bump(); // `<`
        let mut args = Vec::new();
        while !self.check(&K::Gt) {
            match self.ty() {
                Ok(t) => args.push(t),
                Err(_) => {
                    self.pos = save;
                    return None;
                }
            }
            if !self.eat(&K::Comma) {
                break;
            }
        }
        if args.is_empty()
            || !self.eat(&K::Gt)
            || !matches!(self.kind(), K::Dot | K::LParen)
        {
            self.pos = save;
            return None;
        }
        Some(args)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let start = self.span().start;
        match self.kind().clone() {
            K::Int { .. }
            | K::Float { .. }
            | K::Imag { .. }
            | K::Str(_)
            | K::Char(_)
            | K::True
            | K::False => self.literal(),

            K::Ident(name) => {
                let id = self.ident("a name")?;
                if self.check(&K::Lt) {
                    if let Some(args) = self.try_type_args() {
                        return Ok(Expr {
                            span: Span::new(start, self.prev_end()),
                            kind: ExprKind::TypeApp { name: id, args },
                        });
                    }
                }
                Ok(Expr {
                    span: id.span,
                    kind: ExprKind::Ident(name),
                })
            }

            K::LParen => {
                self.bump();
                let mut elems = Vec::new();
                let mut trailing_comma = false;
                while !self.check(&K::RParen) {
                    elems.push(self.expr()?);
                    trailing_comma = self.eat(&K::Comma);
                    if !trailing_comma {
                        break;
                    }
                }
                self.expect(&K::RParen, "`)`")?;
                if elems.len() == 1 && !trailing_comma {
                    return Ok(elems.pop().unwrap());
                }
                Ok(Expr {
                    span: Span::new(start, self.prev_end()),
                    kind: ExprKind::Tuple(elems),
                })
            }

            K::LBracket => {
                self.bump();
                if self.eat(&K::RBracket) {
                    return Ok(Expr {
                        span: Span::new(start, self.prev_end()),
                        kind: ExprKind::Array(Vec::new()),
                    });
                }
                let first = self.expr()?;
                if self.eat(&K::Semi) {
                    let len = self.expr()?;
                    self.expect(&K::RBracket, "`]`")?;
                    return Ok(Expr {
                        span: Span::new(start, self.prev_end()),
                        kind: ExprKind::ArrayRepeat {
                            value: Box::new(first),
                            len: Box::new(len),
                        },
                    });
                }
                let mut elems = vec![first];
                while self.eat(&K::Comma) {
                    if self.check(&K::RBracket) {
                        break;
                    }
                    elems.push(self.expr()?);
                }
                self.expect(&K::RBracket, "`]`")?;
                Ok(Expr {
                    span: Span::new(start, self.prev_end()),
                    kind: ExprKind::Array(elems),
                })
            }

            K::LBrace => {
                let b = self.block()?;
                Ok(Expr {
                    span: b.span,
                    kind: ExprKind::Block(b),
                })
            }

            K::If => self.if_expr(),

            K::Match => {
                self.bump();
                let scrutinee = self.expr()?;
                self.expect(&K::LBrace, "`{` to start the match arms")?;
                self.skip_newlines();
                let mut arms = Vec::new();
                while !self.check(&K::RBrace) {
                    let pat = self.pattern()?;
                    let guard = if self.eat(&K::If) {
                        Some(self.expr()?)
                    } else {
                        None
                    };
                    self.expect(&K::FatArrow, "`=>`")?;
                    let body = self.expr()?;
                    arms.push(MatchArm { pat, guard, body });
                    self.list_sep(&K::RBrace)?;
                }
                self.expect(&K::RBrace, "`}`")?;
                Ok(Expr {
                    span: Span::new(start, self.prev_end()),
                    kind: ExprKind::Match {
                        scrutinee: Box::new(scrutinee),
                        arms,
                    },
                })
            }

            K::For => self.for_expr(false),

            K::While => {
                self.bump();
                let cond = self.expr()?;
                let body = self.block()?;
                Ok(Expr {
                    span: Span::new(start, self.prev_end()),
                    kind: ExprKind::While {
                        cond: Box::new(cond),
                        body,
                    },
                })
            }

            K::Loop => {
                self.bump();
                let body = self.block()?;
                Ok(Expr {
                    span: Span::new(start, self.prev_end()),
                    kind: ExprKind::Loop(body),
                })
            }

            K::Unsafe => {
                self.bump();
                let body = self.block()?;
                Ok(Expr {
                    span: Span::new(start, self.prev_end()),
                    kind: ExprKind::Unsafe(body),
                })
            }

            K::Spawn => {
                self.bump();
                let body = self.block()?;
                Ok(Expr {
                    span: Span::new(start, self.prev_end()),
                    kind: ExprKind::Spawn(body),
                })
            }

            K::On => {
                self.bump();
                let target = self.expr()?;
                let body = self.block()?;
                Ok(Expr {
                    span: Span::new(start, self.prev_end()),
                    kind: ExprKind::On {
                        target: Box::new(target),
                        body,
                    },
                })
            }

            K::Par => {
                self.bump();
                match self.kind().clone() {
                    K::For => {
                        let mut e = self.for_expr(true)?;
                        e.span.start = start;
                        Ok(e)
                    }
                    K::Ident(_) if *self.kind_at(1) == K::LParen => {
                        let op = self.ident("a reduction name")?;
                        self.expect(&K::LParen, "`(`")?;
                        let pat = self.pattern_primary()?;
                        self.expect(&K::In, "`in`")?;
                        let iter = self.expr()?;
                        self.expect(&K::RParen, "`)`")?;
                        let body = self.range()?;
                        Ok(Expr {
                            span: Span::new(start, self.prev_end()),
                            kind: ExprKind::ParReduce {
                                op,
                                pat,
                                iter: Box::new(iter),
                                body: Box::new(body),
                            },
                        })
                    }
                    _ => Err(self.err_expected(
                        "`for` or a reduction like `sum(i in 0..n) ...` after `par`",
                    )),
                }
            }

            K::Return => {
                self.bump();
                let value = if Self::can_start_expr(self.kind()) {
                    Some(Box::new(self.expr()?))
                } else {
                    None
                };
                Ok(Expr {
                    span: Span::new(start, self.prev_end()),
                    kind: ExprKind::Return(value),
                })
            }

            K::Break => {
                self.bump();
                let value = if Self::can_start_expr(self.kind()) {
                    Some(Box::new(self.expr()?))
                } else {
                    None
                };
                Ok(Expr {
                    span: Span::new(start, self.prev_end()),
                    kind: ExprKind::Break(value),
                })
            }

            K::Continue => {
                let span = self.bump().span;
                Ok(Expr {
                    span,
                    kind: ExprKind::Continue,
                })
            }

            K::Pipe | K::PipePipe => self.closure(),

            K::Semi => Err(self.semicolon_error()),

            _ => Err(self.err_expected("an expression")),
        }
    }

    fn if_expr(&mut self) -> PResult<Expr> {
        let start = self.span().start;
        self.expect(&K::If, "`if`")?;
        let cond = self.expr()?;
        let then = self.block()?;
        // `else` may sit on the line after the closing brace
        let save = self.pos;
        self.skip_newlines();
        let else_ = if self.eat(&K::Else) {
            if self.check(&K::If) {
                Some(Box::new(self.if_expr()?))
            } else {
                let b = self.block()?;
                Some(Box::new(Expr {
                    span: b.span,
                    kind: ExprKind::Block(b),
                }))
            }
        } else {
            self.pos = save;
            None
        };
        Ok(Expr {
            span: Span::new(start, self.prev_end()),
            kind: ExprKind::If {
                cond: Box::new(cond),
                then,
                else_,
            },
        })
    }

    fn for_expr(&mut self, par: bool) -> PResult<Expr> {
        let start = self.span().start;
        self.expect(&K::For, "`for`")?;
        let pat = self.pattern()?;
        self.expect(&K::In, "`in`")?;
        let iter = self.expr()?;
        let guard = if self.eat(&K::Where) {
            Some(Box::new(self.expr()?))
        } else {
            None
        };
        let body = self.block()?;
        Ok(Expr {
            span: Span::new(start, self.prev_end()),
            kind: ExprKind::For {
                par,
                pat,
                iter: Box::new(iter),
                guard,
                body,
            },
        })
    }

    fn closure(&mut self) -> PResult<Expr> {
        let start = self.span().start;
        let mut params = Vec::new();
        if self.eat(&K::PipePipe) {
            // `||` : no parameters
        } else {
            self.expect(&K::Pipe, "`|`")?;
            while !self.check(&K::Pipe) {
                let pat = self.pattern_primary()?;
                let ty = if self.eat(&K::Colon) {
                    Some(self.ty()?)
                } else {
                    None
                };
                params.push(ClosureParam { pat, ty });
                if !self.eat(&K::Comma) {
                    break;
                }
            }
            self.expect(&K::Pipe, "`|` to end the closure parameters")?;
        }
        let ret = if self.eat(&K::Arrow) {
            Some(self.ty()?)
        } else {
            None
        };
        let body = if ret.is_some() {
            let b = self.block()?;
            Expr {
                span: b.span,
                kind: ExprKind::Block(b),
            }
        } else {
            self.expr()?
        };
        Ok(Expr {
            span: Span::new(start, self.prev_end()),
            kind: ExprKind::Closure {
                params,
                ret,
                body: Box::new(body),
            },
        })
    }
}
