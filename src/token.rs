//! Tokens produced by the lexer.

use crate::span::Span;

#[derive(Clone, Debug, PartialEq)]
pub enum StrPart {
    /// Literal text (escapes already resolved).
    Lit(String),
    /// An interpolated expression `{expr}`: its source text and the byte offset
    /// of that text in the original file (so errors point at the right place).
    Interp { src: String, offset: usize },
}

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    // Literals and names
    Ident(String),
    Int { value: u128, suffix: Option<String> },
    Float { value: f64, suffix: Option<String> },
    /// Imaginary literal such as `4i`.
    Imag { value: f64 },
    Str(Vec<StrPart>),
    Char(char),

    // Keywords
    Fn,
    Let,
    Mut,
    Const,
    Struct,
    Enum,
    Trait,
    Impl,
    Use,
    Pub,
    If,
    Else,
    Match,
    For,
    While,
    Loop,
    Break,
    Continue,
    Return,
    Own,
    Par,
    Spawn,
    On,
    Unsafe,
    Type,
    As,
    In,
    True,
    False,
    Where,
    Cluster,

    // Delimiters and punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Semi,
    Dot,
    DotDot,
    DotDotEq,
    Arrow,
    FatArrow,
    Question,
    Hash,
    Underscore,

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    At,
    Caret,
    CaretCaret,
    Amp,
    Pipe,
    Tilde,
    Bang,
    Shl,
    AmpAmp,
    PipePipe,
    EqEq,
    BangEq,
    Lt,
    Gt,
    Le,
    Ge,

    // Assignment
    Eq,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    AtEq,
    CaretEq,
    AmpEq,
    PipeEq,

    /// A significant line break (statement terminator).
    Newline,
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

pub fn keyword(s: &str) -> Option<TokenKind> {
    use TokenKind::*;
    Some(match s {
        "fn" => Fn,
        "let" => Let,
        "mut" => Mut,
        "const" => Const,
        "struct" => Struct,
        "enum" => Enum,
        "trait" => Trait,
        "impl" => Impl,
        "use" => Use,
        "pub" => Pub,
        "if" => If,
        "else" => Else,
        "match" => Match,
        "for" => For,
        "while" => While,
        "loop" => Loop,
        "break" => Break,
        "continue" => Continue,
        "return" => Return,
        "own" => Own,
        "par" => Par,
        "spawn" => Spawn,
        "on" => On,
        "unsafe" => Unsafe,
        "type" => Type,
        "as" => As,
        "in" => In,
        "true" => True,
        "false" => False,
        "where" => Where,
        "cluster" => Cluster,
        _ => return None,
    })
}

impl TokenKind {
    /// Source text of a keyword or punctuation token.
    pub fn text(&self) -> &'static str {
        use TokenKind::*;
        match self {
            Fn => "fn",
            Let => "let",
            Mut => "mut",
            Const => "const",
            Struct => "struct",
            Enum => "enum",
            Trait => "trait",
            Impl => "impl",
            Use => "use",
            Pub => "pub",
            If => "if",
            Else => "else",
            Match => "match",
            For => "for",
            While => "while",
            Loop => "loop",
            Break => "break",
            Continue => "continue",
            Return => "return",
            Own => "own",
            Par => "par",
            Spawn => "spawn",
            On => "on",
            Unsafe => "unsafe",
            Type => "type",
            As => "as",
            In => "in",
            True => "true",
            False => "false",
            Where => "where",
            Cluster => "cluster",
            LParen => "(",
            RParen => ")",
            LBrace => "{",
            RBrace => "}",
            LBracket => "[",
            RBracket => "]",
            Comma => ",",
            Colon => ":",
            Semi => ";",
            Dot => ".",
            DotDot => "..",
            DotDotEq => "..=",
            Arrow => "->",
            FatArrow => "=>",
            Question => "?",
            Hash => "#",
            Underscore => "_",
            Plus => "+",
            Minus => "-",
            Star => "*",
            Slash => "/",
            Percent => "%",
            At => "@",
            Caret => "^",
            CaretCaret => "^^",
            Amp => "&",
            Pipe => "|",
            Tilde => "~",
            Bang => "!",
            Shl => "<<",
            AmpAmp => "&&",
            PipePipe => "||",
            EqEq => "==",
            BangEq => "!=",
            Lt => "<",
            Gt => ">",
            Le => "<=",
            Ge => ">=",
            Eq => "=",
            PlusEq => "+=",
            MinusEq => "-=",
            StarEq => "*=",
            SlashEq => "/=",
            PercentEq => "%=",
            AtEq => "@=",
            CaretEq => "^=",
            AmpEq => "&=",
            PipeEq => "|=",
            Ident(_) | Int { .. } | Float { .. } | Imag { .. } | Str(_) | Char(_) | Newline
            | Eof => "",
        }
    }

    /// Human-readable description for error messages.
    pub fn describe(&self) -> String {
        use TokenKind::*;
        match self {
            Ident(s) => format!("identifier `{s}`"),
            Int { .. } | Float { .. } | Imag { .. } => "a number".to_string(),
            Str(_) => "a string".to_string(),
            Char(_) => "a character".to_string(),
            Newline => "end of line".to_string(),
            Eof => "end of file".to_string(),
            k => format!("`{}`", k.text()),
        }
    }

    /// If a line ends with this token, the statement obviously continues on the
    /// next line, so the lexer does not emit a `Newline`.
    pub fn continues_line(&self) -> bool {
        use TokenKind::*;
        matches!(
            self,
            Comma
                | Plus
                | Minus
                | Star
                | Slash
                | Percent
                | At
                | Caret
                | CaretCaret
                | Amp
                | Pipe
                | AmpAmp
                | PipePipe
                | EqEq
                | BangEq
                | Le
                | Ge
                | Shl
                | Eq
                | PlusEq
                | MinusEq
                | StarEq
                | SlashEq
                | PercentEq
                | AtEq
                | CaretEq
                | AmpEq
                | PipeEq
                | Arrow
                | FatArrow
                | Dot
        )
    }
}
