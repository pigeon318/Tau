//! Lexer: source text -> tokens.
//!
//! Tau has no semicolons, so the lexer decides which line breaks are
//! statement terminators (`Newline` tokens). A line break is *not* significant when:
//!   * we are directly inside `(...)` or `[...]` (inside `{...}` it is significant),
//!   * the previous token obviously continues the line (a binary operator, `,`, `=`, `->`, ...),
//!   * the next line starts with `.` (a method chain; `..` is not a chain), or
//!   * there is nothing before it, or the previous token is already a `Newline`.

use crate::diag::Diagnostic;
use crate::span::Span;
use crate::token::{keyword, StrPart, Token, TokenKind};

type LResult<T> = Result<T, Diagnostic>;

pub fn lex(src: &str) -> LResult<Vec<Token>> {
    lex_at(src, 0)
}

/// Lex `src`, adding `base` to every span (used for string interpolations, whose
/// text is a slice of a larger file).
pub fn lex_at(src: &str, base: usize) -> LResult<Vec<Token>> {
    let mut lx = Lexer {
        src,
        chars: src.char_indices().collect(),
        i: 0,
        base,
        tokens: Vec::new(),
        stack: Vec::new(),
    };
    lx.run()?;
    Ok(lx.tokens)
}

struct Lexer<'a> {
    src: &'a str,
    chars: Vec<(usize, char)>,
    i: usize,
    base: usize,
    tokens: Vec<Token>,
    /// Open delimiters, with the span of the opener.
    stack: Vec<(char, Span)>,
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

const INT_SUFFIXES: &[&str] = &[
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
];

impl<'a> Lexer<'a> {
    fn peek(&self, n: usize) -> Option<char> {
        self.chars.get(self.i + n).map(|c| c.1)
    }

    /// Byte offset (within `src`) of char index `idx`.
    fn raw(&self, idx: usize) -> usize {
        self.chars.get(idx).map(|c| c.0).unwrap_or(self.src.len())
    }

    /// Absolute byte offset of char index `idx`.
    fn off(&self, idx: usize) -> usize {
        self.base + self.raw(idx)
    }

    /// `line:col` (1-based) of an absolute byte offset, for messages.
    fn line_col(&self, abs: usize) -> String {
        let raw = abs.saturating_sub(self.base).min(self.src.len());
        let prefix = &self.src[..raw];
        let line = prefix.matches('\n').count() + 1;
        let col = prefix.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        format!("{line}:{col}")
    }

    fn span_from(&self, start_idx: usize) -> Span {
        Span::new(self.off(start_idx), self.off(self.i))
    }

    fn push(&mut self, kind: TokenKind, start_idx: usize) {
        let span = self.span_from(start_idx);
        self.tokens.push(Token { kind, span });
    }

    fn run(&mut self) -> LResult<()> {
        while let Some(c) = self.peek(0) {
            match c {
                ' ' | '\t' | '\r' => self.i += 1,
                '\n' => self.newline(),
                '/' if self.peek(1) == Some('/') => {
                    while let Some(c) = self.peek(0) {
                        if c == '\n' {
                            break;
                        }
                        self.i += 1;
                    }
                }
                '/' if self.peek(1) == Some('*') => self.block_comment()?,
                '"' => self.string()?,
                '\'' => self.char_lit()?,
                c if c.is_ascii_digit() => self.number()?,
                c if is_ident_start(c) => self.ident(),
                _ => self.punct()?,
            }
        }
        if let Some((open, span)) = self.stack.last() {
            return Err(Diagnostic::new(format!("unclosed `{open}`"), *span)
                .with_hint("add the matching closing delimiter"));
        }
        let end = self.off(self.chars.len());
        self.tokens.push(Token {
            kind: TokenKind::Eof,
            span: Span::new(end, end),
        });
        Ok(())
    }

    // ---- whitespace and comments -------------------------------------------------

    /// Index of the first char at or after `j` that is not whitespace/comment.
    /// Used only for look-ahead past a line break.
    fn skip_trivia_from(&self, mut j: usize) -> usize {
        let at = |k: usize| self.chars.get(k).map(|c| c.1);
        loop {
            match at(j) {
                Some(' ') | Some('\t') | Some('\r') | Some('\n') => j += 1,
                Some('/') if at(j + 1) == Some('/') => {
                    while let Some(c) = at(j) {
                        if c == '\n' {
                            break;
                        }
                        j += 1;
                    }
                }
                Some('/') if at(j + 1) == Some('*') => {
                    let mut depth = 0usize;
                    while j < self.chars.len() {
                        if at(j) == Some('/') && at(j + 1) == Some('*') {
                            depth += 1;
                            j += 2;
                        } else if at(j) == Some('*') && at(j + 1) == Some('/') {
                            depth -= 1;
                            j += 2;
                            if depth == 0 {
                                break;
                            }
                        } else {
                            j += 1;
                        }
                    }
                }
                _ => return j,
            }
        }
    }

    fn block_comment(&mut self) -> LResult<()> {
        let start = self.i;
        let mut depth = 0usize;
        loop {
            match (self.peek(0), self.peek(1)) {
                (Some('/'), Some('*')) => {
                    depth += 1;
                    self.i += 2;
                }
                (Some('*'), Some('/')) => {
                    depth -= 1;
                    self.i += 2;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                (Some(_), _) => self.i += 1,
                (None, _) => {
                    let sp = Span::new(self.off(start), self.off(start + 2));
                    return Err(Diagnostic::new("unterminated block comment", sp)
                        .with_hint("close it with `*/`"));
                }
            }
        }
    }

    fn newline(&mut self) {
        let start = self.i;
        self.i += 1;
        if matches!(self.stack.last(), Some(('(', _)) | Some(('[', _))) {
            return;
        }
        match self.tokens.last() {
            None => return,
            Some(t) if t.kind == TokenKind::Newline => return,
            // `use a.*` ends with `*`, which is not a continuation there.
            Some(t) if t.kind == TokenKind::Star => {
                let n = self.tokens.len();
                let after_dot = n >= 2 && self.tokens[n - 2].kind == TokenKind::Dot;
                if !after_dot {
                    return;
                }
            }
            Some(t) if t.kind.continues_line() => return,
            _ => {}
        }
        let j = self.skip_trivia_from(self.i);
        let at = |k: usize| self.chars.get(k).map(|c| c.1);
        if at(j) == Some('.') && at(j + 1) != Some('.') {
            return; // method chain on the next line
        }
        let span = Span::new(self.off(start), self.off(start + 1));
        self.tokens.push(Token {
            kind: TokenKind::Newline,
            span,
        });
    }

    // ---- identifiers -------------------------------------------------------------

    fn ident(&mut self) {
        let start = self.i;
        while matches!(self.peek(0), Some(c) if is_ident_continue(c)) {
            self.i += 1;
        }
        let text = &self.src[self.raw(start)..self.raw(self.i)];
        let kind = if text == "_" {
            TokenKind::Underscore
        } else if let Some(k) = keyword(text) {
            k
        } else {
            TokenKind::Ident(text.to_string())
        };
        self.push(kind, start);
    }

    // ---- numbers -----------------------------------------------------------------

    fn digits(&mut self, radix: u32) {
        while matches!(self.peek(0), Some(c) if c == '_' || c.is_digit(radix)) {
            self.i += 1;
        }
    }

    fn suffix(&mut self) -> Option<String> {
        let s = self.i;
        if matches!(self.peek(0), Some(c) if c.is_alphabetic()) {
            while matches!(self.peek(0), Some(c) if is_ident_continue(c)) {
                self.i += 1;
            }
            Some(self.src[self.raw(s)..self.raw(self.i)].to_string())
        } else {
            None
        }
    }

    fn number(&mut self) -> LResult<()> {
        let start = self.i;
        let prev_dot = matches!(self.tokens.last(), Some(t) if t.kind == TokenKind::Dot);

        // Radix integers: 0xFF, 0b1010, 0o17
        if !prev_dot && self.peek(0) == Some('0') {
            let radix = match self.peek(1) {
                Some('x') => Some(16),
                Some('b') => Some(2),
                Some('o') => Some(8),
                _ => None,
            };
            if let Some(radix) = radix {
                self.i += 2;
                let ds = self.i;
                self.digits(radix);
                let text: String = self.src[self.raw(ds)..self.raw(self.i)]
                    .chars()
                    .filter(|&c| c != '_')
                    .collect();
                if text.is_empty() {
                    return Err(Diagnostic::new(
                        "expected digits after the radix prefix",
                        self.span_from(start),
                    ));
                }
                let value = u128::from_str_radix(&text, radix).map_err(|_| {
                    Diagnostic::new("integer literal is too large", self.span_from(start))
                })?;
                let suffix_start = self.i;
                let suffix = self.suffix();
                if let Some(s) = &suffix {
                    if !INT_SUFFIXES.contains(&s.as_str()) {
                        return Err(Diagnostic::new(
                            format!("invalid integer suffix `{s}`"),
                            Span::new(self.off(suffix_start), self.off(self.i)),
                        ));
                    }
                }
                self.push(TokenKind::Int { value, suffix }, start);
                return Ok(());
            }
        }

        self.digits(10);

        // After a `.` (tuple field like `t.0.1`) only plain integers are allowed.
        if prev_dot {
            let text: String = self.src[self.raw(start)..self.raw(self.i)]
                .chars()
                .filter(|&c| c != '_')
                .collect();
            let value = text.parse::<u128>().map_err(|_| {
                Diagnostic::new("integer literal is too large", self.span_from(start))
            })?;
            self.push(TokenKind::Int { value, suffix: None }, start);
            return Ok(());
        }

        let mut is_float = false;
        if self.peek(0) == Some('.') && matches!(self.peek(1), Some(c) if c.is_ascii_digit()) {
            self.i += 1;
            self.digits(10);
            is_float = true;
        }
        if matches!(self.peek(0), Some('e') | Some('E')) {
            let sign = matches!(self.peek(1), Some('+') | Some('-'));
            let first = if sign { self.peek(2) } else { self.peek(1) };
            if matches!(first, Some(c) if c.is_ascii_digit()) {
                self.i += if sign { 2 } else { 1 };
                self.digits(10);
                is_float = true;
            }
        }
        let text: String = self.src[self.raw(start)..self.raw(self.i)]
            .chars()
            .filter(|&c| c != '_')
            .collect();
        let suffix_start = self.i;
        let suffix = self.suffix();
        let suffix_span = Span::new(self.off(suffix_start), self.off(self.i));

        let float_value = |text: &str, sp: Span| {
            text.parse::<f64>()
                .map_err(|_| Diagnostic::new("invalid number", sp))
        };

        match suffix.as_deref() {
            Some("i") => {
                let value = float_value(&text, self.span_from(start))?;
                self.push(TokenKind::Imag { value }, start);
            }
            Some(s @ ("f32" | "f64")) => {
                let value = float_value(&text, self.span_from(start))?;
                self.push(
                    TokenKind::Float {
                        value,
                        suffix: Some(s.to_string()),
                    },
                    start,
                );
            }
            Some(s) if !is_float && INT_SUFFIXES.contains(&s) => {
                let value = text.parse::<u128>().map_err(|_| {
                    Diagnostic::new("integer literal is too large", self.span_from(start))
                })?;
                self.push(
                    TokenKind::Int {
                        value,
                        suffix: Some(s.to_string()),
                    },
                    start,
                );
            }
            Some(s) => {
                return Err(Diagnostic::new(
                    format!("invalid numeric suffix `{s}`"),
                    suffix_span,
                )
                .with_hint(
                    "valid suffixes are u8..u128, i8..i128, usize, isize, f32, f64 and `i` (imaginary)",
                ));
            }
            None if is_float => {
                let value = float_value(&text, self.span_from(start))?;
                self.push(TokenKind::Float { value, suffix: None }, start);
            }
            None => {
                let value = text.parse::<u128>().map_err(|_| {
                    Diagnostic::new("integer literal is too large", self.span_from(start))
                })?;
                self.push(TokenKind::Int { value, suffix: None }, start);
            }
        }
        Ok(())
    }

    // ---- strings and chars ---------------------------------------------------------

    /// Parse an escape sequence; `self.i` is at the backslash.
    fn escape(&mut self) -> LResult<char> {
        let start = self.i;
        self.i += 1;
        let Some(c) = self.peek(0) else {
            return Err(Diagnostic::new(
                "unterminated escape sequence",
                self.span_from(start),
            ));
        };
        self.i += 1;
        Ok(match c {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '0' => '\0',
            '\\' => '\\',
            '"' => '"',
            '\'' => '\'',
            '{' => '{',
            '}' => '}',
            'u' => {
                if self.peek(0) != Some('{') {
                    return Err(Diagnostic::new(
                        "expected `{` after `\\u`",
                        self.span_from(start),
                    ));
                }
                self.i += 1;
                let ds = self.i;
                while matches!(self.peek(0), Some(c) if c.is_ascii_hexdigit()) {
                    self.i += 1;
                }
                let hex = &self.src[self.raw(ds)..self.raw(self.i)];
                if self.peek(0) != Some('}') || hex.is_empty() {
                    return Err(Diagnostic::new(
                        "malformed unicode escape, expected `\\u{XXXX}`",
                        self.span_from(start),
                    ));
                }
                let code = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
                self.i += 1;
                match code {
                    Some(ch) => ch,
                    None => {
                        return Err(Diagnostic::new(
                            "invalid unicode code point",
                            self.span_from(start),
                        ))
                    }
                }
            }
            other => {
                return Err(Diagnostic::new(
                    format!("unknown escape `\\{other}`"),
                    self.span_from(start),
                ))
            }
        })
    }

    fn string(&mut self) -> LResult<()> {
        let start = self.i;
        self.i += 1;
        let mut parts: Vec<StrPart> = Vec::new();
        let mut cur = String::new();
        loop {
            match self.peek(0) {
                None => {
                    let sp = Span::new(self.off(start), self.off(start + 1));
                    return Err(Diagnostic::new("unterminated string", sp)
                        .with_hint("add a closing `\"`"));
                }
                Some('"') => {
                    self.i += 1;
                    break;
                }
                Some('\\') => {
                    let c = self.escape()?;
                    cur.push(c);
                }
                Some('{') => {
                    if !cur.is_empty() {
                        parts.push(StrPart::Lit(std::mem::take(&mut cur)));
                    }
                    let open = self.i;
                    self.i += 1;
                    let expr_start = self.i;
                    let mut depth = 1usize;
                    loop {
                        match self.peek(0) {
                            None => {
                                let sp = Span::new(self.off(open), self.off(open + 1));
                                return Err(Diagnostic::new("unterminated `{` in string", sp)
                                    .with_hint("use `\\{` for a literal brace"));
                            }
                            Some('{') => {
                                depth += 1;
                                self.i += 1;
                            }
                            Some('}') => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                                self.i += 1;
                            }
                            Some('"') => {
                                // nested string inside the interpolation
                                self.i += 1;
                                while let Some(c) = self.peek(0) {
                                    self.i += 1;
                                    if c == '\\' {
                                        self.i += 1;
                                    } else if c == '"' {
                                        break;
                                    }
                                }
                            }
                            Some(_) => self.i += 1,
                        }
                    }
                    let text = &self.src[self.raw(expr_start)..self.raw(self.i)];
                    if text.trim().is_empty() {
                        let sp = Span::new(self.off(open), self.off(self.i + 1));
                        return Err(Diagnostic::new("empty `{}` in string", sp)
                            .with_hint("put an expression inside, or use `\\{` for a literal brace"));
                    }
                    parts.push(StrPart::Interp {
                        src: text.to_string(),
                        offset: self.off(expr_start),
                    });
                    self.i += 1; // closing brace
                }
                Some(c) => {
                    cur.push(c);
                    self.i += 1;
                }
            }
        }
        if !cur.is_empty() || parts.is_empty() {
            parts.push(StrPart::Lit(cur));
        }
        self.push(TokenKind::Str(parts), start);
        Ok(())
    }

    fn char_lit(&mut self) -> LResult<()> {
        let start = self.i;
        self.i += 1;
        let c = match self.peek(0) {
            Some('\\') => self.escape()?,
            Some('\'') | Some('\n') | None => {
                return Err(Diagnostic::new(
                    "empty or unterminated character literal",
                    self.span_from(start),
                ))
            }
            Some(c) => {
                self.i += 1;
                c
            }
        };
        if self.peek(0) != Some('\'') {
            return Err(Diagnostic::new(
                "unterminated character literal",
                self.span_from(start),
            )
            .with_hint("character literals hold exactly one character, like 'a'"));
        }
        self.i += 1;
        self.push(TokenKind::Char(c), start);
        Ok(())
    }

    // ---- punctuation ----------------------------------------------------------------

    fn punct(&mut self) -> LResult<()> {
        use TokenKind::*;
        let start = self.i;
        let c = self.peek(0).unwrap();
        let c1 = self.peek(1);
        let c2 = self.peek(2);
        let (kind, len) = match (c, c1, c2) {
            ('.', Some('.'), Some('=')) => (DotDotEq, 3),
            ('.', Some('.'), _) => (DotDot, 2),
            ('.', _, _) => (Dot, 1),
            ('-', Some('>'), _) => (Arrow, 2),
            ('-', Some('='), _) => (MinusEq, 2),
            ('-', _, _) => (Minus, 1),
            ('=', Some('>'), _) => (FatArrow, 2),
            ('=', Some('='), _) => (EqEq, 2),
            ('=', _, _) => (Eq, 1),
            ('+', Some('='), _) => (PlusEq, 2),
            ('+', _, _) => (Plus, 1),
            ('*', Some('='), _) => (StarEq, 2),
            ('*', _, _) => (Star, 1),
            ('/', Some('='), _) => (SlashEq, 2),
            ('/', _, _) => (Slash, 1),
            ('%', Some('='), _) => (PercentEq, 2),
            ('%', _, _) => (Percent, 1),
            ('@', Some('='), _) => (AtEq, 2),
            ('@', _, _) => (At, 1),
            ('^', Some('^'), _) => (CaretCaret, 2),
            ('^', Some('='), _) => (CaretEq, 2),
            ('^', _, _) => (Caret, 1),
            ('&', Some('&'), _) => (AmpAmp, 2),
            ('&', Some('='), _) => (AmpEq, 2),
            ('&', _, _) => (Amp, 1),
            ('|', Some('|'), _) => (PipePipe, 2),
            ('|', Some('='), _) => (PipeEq, 2),
            ('|', _, _) => (Pipe, 1),
            ('<', Some('<'), _) => (Shl, 2),
            ('<', Some('='), _) => (Le, 2),
            ('<', _, _) => (Lt, 1),
            // `>>` is lexed as two `Gt` tokens so nested generics (`Vec<Vec<f64>>`)
            // work; the parser re-joins adjacent `Gt Gt` into a shift.
            ('>', Some('='), _) => (Ge, 2),
            ('>', _, _) => (Gt, 1),
            ('!', Some('='), _) => (BangEq, 2),
            ('!', _, _) => (Bang, 1),
            ('~', _, _) => (Tilde, 1),
            (',', _, _) => (Comma, 1),
            (':', _, _) => (Colon, 1),
            (';', _, _) => (Semi, 1),
            ('?', _, _) => (Question, 1),
            ('#', _, _) => (Hash, 1),
            ('(', _, _) => (LParen, 1),
            (')', _, _) => (RParen, 1),
            ('{', _, _) => (LBrace, 1),
            ('}', _, _) => (RBrace, 1),
            ('[', _, _) => (LBracket, 1),
            (']', _, _) => (RBracket, 1),
            (other, _, _) => {
                return Err(Diagnostic::new(
                    format!("unexpected character `{other}`"),
                    Span::new(self.off(start), self.off(start + 1)),
                ))
            }
        };
        self.i += len;
        let span = self.span_from(start);
        match c {
            '(' | '[' | '{' => self.stack.push((c, span)),
            ')' | ']' | '}' => {
                let want = match c {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                match self.stack.pop() {
                    Some((open, _)) if open == want => {}
                    Some((open, open_span)) => {
                        let close = match open {
                            '(' => ')',
                            '[' => ']',
                            _ => '}',
                        };
                        return Err(Diagnostic::new(
                            format!("mismatched closing `{c}`, expected `{close}`"),
                            span,
                        )
                        .with_hint(format!(
                            "the `{open}` opened at {} is still open",
                            self.line_col(open_span.start)
                        )));
                    }
                    None => {
                        return Err(Diagnostic::new(
                            format!("unmatched closing `{c}`"),
                            span,
                        ))
                    }
                }
            }
            _ => {}
        }
        self.tokens.push(Token { kind, span });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind as K;

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src)
            .unwrap()
            .into_iter()
            .map(|t| t.kind)
            .filter(|k| *k != K::Eof)
            .collect()
    }

    fn id(s: &str) -> TokenKind {
        K::Ident(s.to_string())
    }

    fn int(v: u128) -> TokenKind {
        K::Int { value: v, suffix: None }
    }

    #[test]
    fn identifiers_and_keywords() {
        assert_eq!(kinds("let mut x_1"), vec![K::Let, K::Mut, id("x_1")]);
        assert_eq!(kinds("_ _x"), vec![K::Underscore, id("_x")]);
        assert_eq!(kinds("Δt θ"), vec![id("Δt"), id("θ")]);
    }

    #[test]
    fn numbers() {
        assert_eq!(kinds("42 1_000"), vec![int(42), int(1000)]);
        assert_eq!(kinds("0xFF 0b1010 0o17"), vec![int(255), int(10), int(15)]);
        assert_eq!(
            kinds("3.14 1e-9 2.5e3"),
            vec![
                K::Float { value: 3.14, suffix: None },
                K::Float { value: 1e-9, suffix: None },
                K::Float { value: 2500.0, suffix: None },
            ]
        );
        assert_eq!(
            kinds("5u8 2.0f32 7f64"),
            vec![
                K::Int { value: 5, suffix: Some("u8".into()) },
                K::Float { value: 2.0, suffix: Some("f32".into()) },
                K::Float { value: 7.0, suffix: Some("f64".into()) },
            ]
        );
        assert_eq!(kinds("4i"), vec![K::Imag { value: 4.0 }]);
    }

    #[test]
    fn ranges_are_not_floats() {
        assert_eq!(kinds("0..10"), vec![int(0), K::DotDot, int(10)]);
        assert_eq!(kinds("1..=5"), vec![int(1), K::DotDotEq, int(5)]);
    }

    #[test]
    fn tuple_fields() {
        assert_eq!(kinds("t.0.1"), vec![id("t"), K::Dot, int(0), K::Dot, int(1)]);
    }

    #[test]
    fn method_call_on_int() {
        assert_eq!(kinds("1.max(2)").len(), 6);
    }

    #[test]
    fn newlines_terminate_statements() {
        assert_eq!(
            kinds("a\nb"),
            vec![id("a"), K::Newline, id("b")]
        );
        // blank lines and comments collapse
        assert_eq!(
            kinds("a\n\n// hi\n\nb"),
            vec![id("a"), K::Newline, id("b")]
        );
    }

    #[test]
    fn newlines_continue_after_operators() {
        assert_eq!(kinds("a +\nb"), vec![id("a"), K::Plus, id("b")]);
        assert_eq!(kinds("f(a,\nb)"), kinds("f(a, b)"));
        assert_eq!(kinds("x =\n1"), vec![id("x"), K::Eq, int(1)]);
    }

    #[test]
    fn newlines_ignored_inside_parens_and_brackets_not_braces() {
        assert_eq!(kinds("(a\nb)"), vec![K::LParen, id("a"), id("b"), K::RParen]);
        assert_eq!(kinds("[a\nb]"), vec![K::LBracket, id("a"), id("b"), K::RBracket]);
        assert_eq!(
            kinds("{a\nb}"),
            vec![K::LBrace, id("a"), K::Newline, id("b"), K::RBrace]
        );
        // a brace block nested inside parens: newlines matter again
        // (the line break right after `{` yields a Newline too; the parser skips it)
        assert_eq!(
            kinds("f(|x| {\na\nb\n})"),
            vec![
                id("f"), K::LParen, K::Pipe, id("x"), K::Pipe, K::LBrace, K::Newline,
                id("a"), K::Newline, id("b"), K::Newline, K::RBrace, K::RParen
            ]
        );
    }

    #[test]
    fn method_chain_on_next_line() {
        assert_eq!(kinds("x\n  .foo()"), kinds("x.foo()"));
        assert_eq!(kinds("x\n  // note\n  .foo()"), kinds("x.foo()"));
        // but a range at the start of the next line is a new statement
        assert_eq!(
            kinds("x\n..y"),
            vec![id("x"), K::Newline, K::DotDot, id("y")]
        );
    }

    #[test]
    fn use_glob_ends_the_line() {
        assert_eq!(
            kinds("use a.*\nb"),
            vec![K::Use, id("a"), K::Dot, K::Star, K::Newline, id("b")]
        );
    }

    #[test]
    fn comments() {
        assert_eq!(kinds("a // x\nb"), vec![id("a"), K::Newline, id("b")]);
        assert_eq!(kinds("a /* x /* nested */ y */ b"), vec![id("a"), id("b")]);
        assert!(lex("/* never closed").is_err());
    }

    #[test]
    fn strings_and_interpolation() {
        assert_eq!(
            kinds(r#""hi {x + 1}!""#),
            vec![K::Str(vec![
                StrPart::Lit("hi ".into()),
                StrPart::Interp { src: "x + 1".into(), offset: 5 },
                StrPart::Lit("!".into()),
            ])]
        );
        assert_eq!(
            kinds(r#""a\n\{b\}""#),
            vec![K::Str(vec![StrPart::Lit("a\n{b}".into())])]
        );
        assert_eq!(kinds(r#""""#), vec![K::Str(vec![StrPart::Lit(String::new())])]);
        // nested quotes inside an interpolation
        let t = kinds(r#""{m["k"]}""#);
        assert_eq!(
            t,
            vec![K::Str(vec![StrPart::Interp { src: r#"m["k"]"#.into(), offset: 2 }])]
        );
    }

    #[test]
    fn chars() {
        assert_eq!(kinds(r"'a' '\n' '\u{3c4}'"), vec![K::Char('a'), K::Char('\n'), K::Char('τ')]);
    }

    #[test]
    fn operators() {
        assert_eq!(
            kinds("a ^^ b ^ c @ d"),
            vec![id("a"), K::CaretCaret, id("b"), K::Caret, id("c"), K::At, id("d")]
        );
        assert_eq!(
            kinds("+= -= *= /= == != <= >= -> =>"),
            vec![
                K::PlusEq, K::MinusEq, K::StarEq, K::SlashEq, K::EqEq, K::BangEq,
                K::Le, K::Ge, K::Arrow, K::FatArrow
            ]
        );
        // `>>` is two Gt tokens, so `Vec<Vec<f64>>` can be parsed
        assert_eq!(kinds(">>"), vec![K::Gt, K::Gt]);
    }

    #[test]
    fn errors() {
        assert!(lex("\"abc").is_err());
        assert!(lex("(]").is_err());
        assert!(lex("(").is_err());
        assert!(lex(")").is_err());
        assert!(lex("5xyz").is_err());
        assert!(lex("\"{}\"").is_err());
        assert!(lex("`").is_err());
        assert!(lex("'ab'").is_err());
    }

    #[test]
    fn spans_are_byte_offsets() {
        let t = lex("ab + cd").unwrap();
        assert_eq!(t[0].span, Span::new(0, 2));
        assert_eq!(t[1].span, Span::new(3, 4));
        assert_eq!(t[2].span, Span::new(5, 7));
    }
}
