//! Format specifications inside string interpolations: `"{x:>8.3}"`.
//!
//! ```text
//! spec      = [[fill]align] [sign] ['#'] ['0'] [width] ['.' precision] [type]
//! fill      = any character              align = '<' | '^' | '>'
//! sign      = '+'                        width, precision = digits
//! type      = 'x' | 'X' | 'b' | 'o'      (integers: hexadecimal, binary, octal)
//!           | 'e' | 'E'                  (floats: scientific notation)
//! ```
//!
//! | spec | meaning |
//! |---|---|
//! | `{x:8}` | at least 8 wide: numbers on the right, text on the left |
//! | `{x:<8}` `{x:^8}` `{x:>8}` | left, centre, right |
//! | `{x:*^9}` | centred, padded with `*` |
//! | `{x:+}` | a sign on positive numbers too |
//! | `{n:08}` | pad a number with zeros, after its sign |
//! | `{x:.3}` | a float with 3 decimals; text cut to 3 characters |
//! | `{n:x}` `{n:#X}` `{n:b}` `{n:#o}` | hexadecimal, binary, octal (a negative number as its two's complement) |
//! | `{x:e}` `{x:.2E}` | scientific notation: `1.5e3`, `1.50E3` |
//!
//! The text is produced by the functions here, in the interpreter, and by a port of them in the
//! C runtime (`tau_fmt_*_spec`); both must give identical results. Width counts characters.

use crate::span::Span;

pub const MAX_WIDTH: u32 = 65535;
pub const MAX_PRECISION: u32 = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FmtType {
    LowerHex,
    UpperHex,
    Binary,
    Octal,
    LowerExp,
    UpperExp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FmtSpec {
    pub fill: char,
    pub align: Option<Align>,
    pub plus: bool,
    pub alt: bool,
    pub zero: bool,
    pub width: Option<u32>,
    pub precision: Option<u32>,
    pub ty: Option<FmtType>,
}

impl Default for FmtSpec {
    fn default() -> FmtSpec {
        FmtSpec { fill: ' ', align: None, plus: false, alt: false, zero: false, width: None, precision: None, ty: None }
    }
}

/// A spec together with where it was written (for error messages).
#[derive(Clone, Debug, PartialEq)]
pub struct Fmt {
    pub spec: FmtSpec,
    pub span: Span,
}

// ------------------------------------------------------------------ parsing

/// Parse the text after the `:` of an interpolation.
pub fn parse(text: &str) -> Result<FmtSpec, String> {
    let cs: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut s = FmtSpec::default();
    let is_align = |c: char| matches!(c, '<' | '^' | '>');
    let to_align = |c: char| match c {
        '<' => Align::Left,
        '^' => Align::Center,
        _ => Align::Right,
    };
    if cs.len() >= 2 && is_align(cs[1]) {
        s.fill = cs[0];
        s.align = Some(to_align(cs[1]));
        i = 2;
    } else if !cs.is_empty() && is_align(cs[0]) {
        s.align = Some(to_align(cs[0]));
        i = 1;
    }
    if cs.get(i) == Some(&'+') {
        s.plus = true;
        i += 1;
    }
    if cs.get(i) == Some(&'#') {
        s.alt = true;
        i += 1;
    }
    if cs.get(i) == Some(&'0') {
        s.zero = true;
        i += 1;
    }
    let number = |i: &mut usize, what: &str, limit: u32| -> Result<Option<u32>, String> {
        let start = *i;
        let mut n: u64 = 0;
        while let Some(d) = cs.get(*i).and_then(|c| c.to_digit(10)) {
            n = n * 10 + d as u64;
            if n > u32::MAX as u64 {
                break;
            }
            *i += 1;
        }
        if *i == start {
            return Ok(None);
        }
        if n > limit as u64 {
            return Err(format!("the {what} is too large (the limit is {limit})"));
        }
        Ok(Some(n as u32))
    };
    s.width = number(&mut i, "width", MAX_WIDTH)?;
    if cs.get(i) == Some(&'.') {
        i += 1;
        s.precision = number(&mut i, "precision", MAX_PRECISION)?;
        if s.precision.is_none() {
            return Err("a `.` must be followed by the number of digits".to_string());
        }
    }
    if let Some(&c) = cs.get(i) {
        s.ty = Some(match c {
            'x' => FmtType::LowerHex,
            'X' => FmtType::UpperHex,
            'b' => FmtType::Binary,
            'o' => FmtType::Octal,
            'e' => FmtType::LowerExp,
            'E' => FmtType::UpperExp,
            _ => return Err(format!("unknown format type `{c}` (use x, X, b, o, e or E)")),
        });
        i += 1;
    }
    if i != cs.len() {
        return Err(format!("unexpected `{}` in the format", cs[i..].iter().collect::<String>()));
    }
    Ok(s)
}

/// Split the source of an interpolation `x + 1:>8.2` into the expression and its spec text: the last
/// `:` outside brackets, strings and characters, whose tail is a valid spec. Returns the byte index of
/// the colon. (A `:` in `|x: i64| ...` is not followed by a valid spec, so it is not taken for one.)
pub fn split(src: &str) -> Option<usize> {
    colons(src).into_iter().rev().find(|&c| parse(&src[c + 1..]).is_ok())
}

/// Byte positions of every `:` at the top level of an expression.
pub fn colons(src: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut it = src.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '"' => {
                // a string, possibly with its own `{...}` and escapes
                while let Some((_, d)) = it.next() {
                    if d == '\\' {
                        it.next();
                    } else if d == '"' {
                        break;
                    }
                }
            }
            '\'' => {
                // a character literal: 'a' or '\n'; otherwise just a quote
                let rest = &src[i + 1..];
                let mut cs = rest.chars();
                let (a, b, c3) = (cs.next(), cs.next(), cs.next());
                match (a, b, c3) {
                    (Some('\\'), Some(_), _) => {
                        for (_, d) in it.by_ref() {
                            if d == '\'' {
                                break;
                            }
                        }
                    }
                    (Some(_), Some('\''), _) => {
                        it.next();
                        it.next();
                    }
                    _ => {}
                }
            }
            ':' if depth == 0 => out.push(i),
            _ => {}
        }
    }
    out
}

// ------------------------------------------------------------------ what a type accepts

/// What kind of value a spec is applied to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Int,
    Float,
    Complex,
    /// `str`, `String`
    Text,
    /// `char`, `bool`
    Word,
    /// anything else that can be displayed: only the padding applies
    Other,
}

/// `Err` says why `spec` cannot be applied to a value of this kind.
pub fn check(kind: Kind, spec: &FmtSpec) -> Result<(), String> {
    let what = match kind {
        Kind::Int => "an integer",
        Kind::Float => "a float",
        Kind::Complex => "a complex number",
        Kind::Text => "text",
        Kind::Word => "a character or bool",
        Kind::Other => "this type",
    };
    let bad = |s: &str| Err(format!("{s} cannot be used on {what}"));
    match kind {
        Kind::Int => {
            if spec.precision.is_some() {
                return bad("a precision (`.N`)");
            }
            if matches!(spec.ty, Some(FmtType::LowerExp | FmtType::UpperExp)) {
                return bad("`e` (scientific notation)");
            }
        }
        Kind::Float => {
            if matches!(spec.ty, Some(FmtType::LowerHex | FmtType::UpperHex | FmtType::Binary | FmtType::Octal)) {
                return bad("`x`, `X`, `b` and `o`");
            }
            if spec.alt {
                return bad("`#`");
            }
        }
        Kind::Complex => {
            if matches!(spec.ty, Some(FmtType::LowerHex | FmtType::UpperHex | FmtType::Binary | FmtType::Octal)) {
                return bad("`x`, `X`, `b` and `o`");
            }
            if spec.plus || spec.zero || spec.alt {
                return bad("`+`, `0` and `#`");
            }
        }
        Kind::Text => {
            if spec.plus || spec.zero || spec.alt || spec.ty.is_some() {
                return bad("`+`, `0`, `#` and a type letter");
            }
        }
        Kind::Word | Kind::Other => {
            if spec.plus || spec.zero || spec.alt || spec.ty.is_some() || spec.precision.is_some() {
                return bad("`+`, `0`, `#`, a precision and a type letter");
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ formatting

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// Pad `text` to the spec's width. `default` is the alignment when the spec names none.
pub fn pad(text: &str, spec: &FmtSpec, default: Align) -> String {
    let width = spec.width.unwrap_or(0) as usize;
    let len = chars(text);
    if len >= width {
        return text.to_string();
    }
    let gap = width - len;
    let (left, right) = match spec.align.unwrap_or(default) {
        Align::Left => (0, gap),
        Align::Right => (gap, 0),
        Align::Center => (gap / 2, gap - gap / 2),
    };
    let mut out = String::with_capacity(text.len() + gap);
    out.extend(std::iter::repeat(spec.fill).take(left));
    out.push_str(text);
    out.extend(std::iter::repeat(spec.fill).take(right));
    out
}

/// A number: a sign, a prefix (`0x`), digits. With the `0` flag the zeros go between the prefix and
/// the digits and the width is reached with them; otherwise the whole is padded.
fn number(sign: &str, prefix: &str, digits: &str, spec: &FmtSpec) -> String {
    if spec.zero {
        let width = spec.width.unwrap_or(0) as usize;
        let used = chars(sign) + chars(prefix) + chars(digits);
        let zeros = width.saturating_sub(used);
        return format!("{sign}{prefix}{}{digits}", "0".repeat(zeros));
    }
    pad(&format!("{sign}{prefix}{digits}"), spec, Align::Right)
}

/// An integer of `bits` bits (`signed` or not).
pub fn format_int(v: i128, bits: u32, signed: bool, spec: &FmtSpec) -> String {
    let radix = match spec.ty {
        Some(FmtType::LowerHex) | Some(FmtType::UpperHex) => 16,
        Some(FmtType::Binary) => 2,
        Some(FmtType::Octal) => 8,
        _ => 10,
    };
    if radix == 10 {
        let sign = if v < 0 {
            "-"
        } else if spec.plus {
            "+"
        } else {
            ""
        };
        return number(sign, "", &v.unsigned_abs().to_string(), spec);
    }
    // the bit pattern of the value in its own width
    let bits = bits.clamp(1, 128);
    let mask: u128 = if bits == 128 { u128::MAX } else { (1u128 << bits) - 1 };
    let _ = signed;
    let u = (v as u128) & mask;
    let mut digits = match radix {
        16 => format!("{u:x}"),
        2 => format!("{u:b}"),
        _ => format!("{u:o}"),
    };
    if spec.ty == Some(FmtType::UpperHex) {
        digits = digits.to_uppercase();
    }
    let prefix = if !spec.alt {
        ""
    } else {
        match spec.ty {
            Some(FmtType::LowerHex) => "0x",
            Some(FmtType::UpperHex) => "0X",
            Some(FmtType::Binary) => "0b",
            _ => "0o",
        }
    };
    number(if spec.plus { "+" } else { "" }, prefix, &digits, spec)
}

/// Digits of a float in scientific notation, as Rust writes them: `1.5e3`, `1.50e-7`.
fn exp_text(mag: f64, single: bool, spec: &FmtSpec) -> String {
    let t = match (spec.precision, single) {
        (Some(p), _) => format!("{:.*e}", p as usize, mag),
        (None, true) => format!("{:e}", mag as f32),
        (None, false) => format!("{mag:e}"),
    };
    if spec.ty == Some(FmtType::UpperExp) {
        t.replace('e', "E")
    } else {
        t
    }
}

/// A float. `plain` is the text of its magnitude when no type or precision asks for something else (the
/// language's own float printing, which the caller owns: shortest digits, `.0` on whole numbers).
pub fn format_float(x: f64, single: bool, plain: &str, spec: &FmtSpec) -> String {
    let nan = x.is_nan();
    let neg = x.is_sign_negative() && !nan;
    let mag = x.abs();
    let body = if nan {
        "NaN".to_string()
    } else if x.is_infinite() {
        "inf".to_string()
    } else if matches!(spec.ty, Some(FmtType::LowerExp | FmtType::UpperExp)) {
        exp_text(mag, single, spec)
    } else if let Some(p) = spec.precision {
        format!("{:.*}", p as usize, mag)
    } else {
        plain.trim_start_matches('-').to_string()
    };
    let sign = if neg {
        "-"
    } else if spec.plus && !nan {
        "+"
    } else {
        ""
    };
    if !x.is_finite() {
        // no zeros in front of `inf` and `NaN`
        return pad(&format!("{sign}{body}"), spec, Align::Right);
    }
    number(sign, "", &body, spec)
}

/// Text (`str`, `String`): `.N` keeps the first N characters; the default alignment is left.
pub fn format_text(s: &str, spec: &FmtSpec) -> String {
    match spec.precision {
        Some(p) => {
            let cut: String = s.chars().take(p as usize).collect();
            pad(&cut, spec, Align::Left)
        }
        None => pad(s, spec, Align::Left),
    }
}

/// Everything else that has text: only the width, fill and alignment apply.
pub fn format_other(text: &str, spec: &FmtSpec) -> String {
    pad(text, spec, Align::Left)
}

/// A complex number `a+bi`: the precision and the type apply to both parts, the width to the whole.
pub fn format_complex(re: f64, im: f64, single: bool, re_plain: &str, im_plain: &str, spec: &FmtSpec) -> String {
    let part = FmtSpec { width: None, align: None, zero: false, plus: false, ..*spec };
    let r = format_float(re, single, re_plain, &part);
    let neg = im.is_sign_negative() && !im.is_nan();
    let i = format_float(im.abs(), single, im_plain.trim_start_matches('-'), &part);
    pad(&format!("{r}{}{i}i", if neg { "-" } else { "+" }), spec, Align::Left)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(t: &str) -> FmtSpec {
        parse(t).unwrap_or_else(|e| panic!("{t}: {e}"))
    }

    #[test]
    fn parsing() {
        assert_eq!(spec(""), FmtSpec::default());
        assert_eq!(spec(">8"), FmtSpec { align: Some(Align::Right), width: Some(8), ..Default::default() });
        assert_eq!(spec("*^9.2"), FmtSpec { fill: '*', align: Some(Align::Center), width: Some(9), precision: Some(2), ..Default::default() });
        assert_eq!(spec("+08.3"), FmtSpec { plus: true, zero: true, width: Some(8), precision: Some(3), ..Default::default() });
        assert_eq!(spec("#x"), FmtSpec { alt: true, ty: Some(FmtType::LowerHex), ..Default::default() });
        assert_eq!(spec("#010b"), FmtSpec { alt: true, zero: true, width: Some(10), ty: Some(FmtType::Binary), ..Default::default() });
        assert_eq!(spec(".2E"), FmtSpec { precision: Some(2), ty: Some(FmtType::UpperExp), ..Default::default() });
        // a fill that is itself an alignment character, and a fill that is a digit
        assert_eq!(spec("<<5"), FmtSpec { fill: '<', align: Some(Align::Left), width: Some(5), ..Default::default() });
        assert_eq!(spec("0>5"), FmtSpec { fill: '0', align: Some(Align::Right), width: Some(5), ..Default::default() });
        assert_eq!(spec("é^5").fill, 'é');
        for bad in [".", ".x", "8.", "q", "5q", "x8", ">>>", "+-", "99999999", ".5000"] {
            assert!(parse(bad).is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn splitting_the_expression_from_the_spec() {
        fn at(s: &str) -> Option<(&str, &str)> {
            split(s).map(|i| (&s[..i], &s[i + 1..]))
        }
        assert_eq!(at("x:.2"), Some(("x", ".2")));
        assert_eq!(at("a.b + 1:>8"), Some(("a.b + 1", ">8")));
        assert_eq!(at("f(1, 2):08.3"), Some(("f(1, 2)", "08.3")));
        assert_eq!(at("m[\"k:v\"]:>5"), Some(("m[\"k:v\"]", ">5")));
        assert_eq!(at("'a':>3"), Some(("'a'", ">3")));
        assert_eq!(at("':':>3"), Some(("':'", ">3")));
        assert_eq!(at("x"), None);
        assert_eq!(at("g(|a: i64| a * 2, 3)"), None);
        assert_eq!(at("|a: i64| a"), None);
        assert_eq!(at("{ x }:>4"), Some(("{ x }", ">4")));
        // the last valid colon wins
        assert_eq!(at("x:3:>4"), Some(("x:3", ">4")));
    }

    #[test]
    fn what_each_kind_accepts() {
        assert!(check(Kind::Int, &spec("08x")).is_ok());
        assert!(check(Kind::Int, &spec(".2")).is_err());
        assert!(check(Kind::Int, &spec("e")).is_err());
        assert!(check(Kind::Float, &spec("+010.3e")).is_ok());
        assert!(check(Kind::Float, &spec("x")).is_err());
        assert!(check(Kind::Float, &spec("#")).is_err());
        assert!(check(Kind::Text, &spec(">10.3")).is_ok());
        assert!(check(Kind::Text, &spec("+")).is_err());
        assert!(check(Kind::Text, &spec("08")).is_err());
        assert!(check(Kind::Word, &spec("^5")).is_ok());
        assert!(check(Kind::Word, &spec(".2")).is_err());
        assert!(check(Kind::Other, &spec("<12")).is_ok());
        assert!(check(Kind::Other, &spec("x")).is_err());
        assert!(check(Kind::Complex, &spec(".2")).is_ok());
        assert!(check(Kind::Complex, &spec("+")).is_err());
    }

    #[test]
    fn integers() {
        assert_eq!(format_int(42, 64, true, &spec("")), "42");
        assert_eq!(format_int(42, 64, true, &spec("6")), "    42");
        assert_eq!(format_int(42, 64, true, &spec("<6")), "42    ");
        assert_eq!(format_int(42, 64, true, &spec("^6")), "  42  ");
        assert_eq!(format_int(42, 64, true, &spec("*^7")), "**42***");
        assert_eq!(format_int(42, 64, true, &spec("+")), "+42");
        assert_eq!(format_int(-42, 64, true, &spec("+6")), "   -42");
        assert_eq!(format_int(42, 64, true, &spec("06")), "000042");
        assert_eq!(format_int(-42, 64, true, &spec("06")), "-00042");
        assert_eq!(format_int(-42, 64, true, &spec("+06")), "-00042");
        assert_eq!(format_int(42, 64, true, &spec("+06")), "+00042");
        assert_eq!(format_int(255, 64, true, &spec("x")), "ff");
        assert_eq!(format_int(255, 64, true, &spec("X")), "FF");
        assert_eq!(format_int(255, 64, true, &spec("#x")), "0xff");
        assert_eq!(format_int(255, 64, true, &spec("#X")), "0XFF");
        assert_eq!(format_int(255, 64, true, &spec("#010x")), "0x000000ff");
        assert_eq!(format_int(5, 64, true, &spec("b")), "101");
        assert_eq!(format_int(5, 8, false, &spec("#010b")), "0b00000101");
        assert_eq!(format_int(8, 64, true, &spec("o")), "10");
        assert_eq!(format_int(8, 64, true, &spec("#o")), "0o10");
        // negative numbers show their bits in the type's width
        assert_eq!(format_int(-1, 32, true, &spec("x")), "ffffffff");
        assert_eq!(format_int(-1, 8, true, &spec("b")), "11111111");
        assert_eq!(format_int(-2, 16, true, &spec("#x")), "0xfffe");
        assert_eq!(format_int(-1, 128, true, &spec("x")), "f".repeat(32));
        assert_eq!(format_int(i128::MIN, 128, true, &spec("")), "-170141183460469231731687303715884105728");
        assert_eq!(format_int(u128::MAX as i128, 128, false, &spec("x")), "f".repeat(32));
        assert_eq!(format_int(255, 8, false, &spec("+x")), "+ff");
        // a number wider than the width is not cut
        assert_eq!(format_int(123456, 64, true, &spec("3")), "123456");
    }

    #[test]
    fn floats() {
        let f = |x: f64, plain: &str, s: &str| format_float(x, false, plain, &spec(s));
        assert_eq!(f(3.14159, "3.14159", ".2"), "3.14");
        assert_eq!(f(3.14159, "3.14159", ".0"), "3");
        assert_eq!(f(2.5, "2.5", ".0"), "2");
        assert_eq!(f(0.125, "0.125", ".2"), "0.12");
        assert_eq!(f(3.0, "3.0", ".3"), "3.000");
        assert_eq!(f(3.14159, "3.14159", "10.2"), "      3.14");
        assert_eq!(f(3.14159, "3.14159", "<10.2"), "3.14      ");
        assert_eq!(f(-3.14159, "-3.14159", "010.2"), "-000003.14");
        assert_eq!(f(3.14159, "3.14159", "+.1"), "+3.1");
        assert_eq!(f(3.0, "3.0", "8"), "     3.0");
        assert_eq!(f(-0.0, "-0.0", ".1"), "-0.0");
        assert_eq!(f(1e21, "1000000000000000000000.0", ".1"), "1000000000000000000000.0");
        assert_eq!(f(f64::NAN, "NaN", "05"), "  NaN");
        assert_eq!(f(f64::INFINITY, "inf", "+6"), "  +inf");
        assert_eq!(f(f64::NEG_INFINITY, "-inf", "06"), "  -inf");
        assert_eq!(f(1500.0, "1500.0", "e"), "1.5e3");
        assert_eq!(f(1500.0, "1500.0", ".2e"), "1.50e3");
        assert_eq!(f(0.00015, "0.00015", "E"), "1.5E-4");
        assert_eq!(f(0.0, "0.0", "e"), "0e0");
        assert_eq!(f(-2.5e-7, "-0.00000025", ".1e"), "-2.5e-7");
        assert_eq!(f(12345.678, "12345.678", "12.1e"), "       1.2e4");
        // single precision: the shortest digits of the f32
        assert_eq!(format_float(0.1f32 as f64, true, "0.1", &spec("e")), "1e-1");
        assert_eq!(format_float(0.1f32 as f64, true, "0.1", &spec(".10")), "0.1000000015");
    }

    #[test]
    fn text_and_complex() {
        assert_eq!(format_text("héllo", &spec("8")), "héllo   ");
        assert_eq!(format_text("héllo", &spec(">8")), "   héllo");
        assert_eq!(format_text("héllo", &spec("^9")), "  héllo  ");
        assert_eq!(format_text("héllo", &spec(".3")), "hél");
        assert_eq!(format_text("héllo", &spec("-<8.3")), "hél-----");
        assert_eq!(format_text("abc", &spec("2")), "abc");
        assert_eq!(format_other("[1, 2]", &spec(">8")), "  [1, 2]");
        assert_eq!(format_complex(1.5, -2.0, false, "1.5", "2.0", &spec(".2")), "1.50-2.00i");
        assert_eq!(format_complex(1.0, 2.0, false, "1.0", "2.0", &spec(">12.1")), "    1.0+2.0i");
    }
}
