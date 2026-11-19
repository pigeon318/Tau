//! The small part of TOML that `tau.toml` and `tau.lock` use: tables (`[a]`, `[a.b]`, `[[a]]`),
//! `key = value`, strings, integers, booleans, arrays (also over several lines), inline tables,
//! and `#` comments. Dotted keys, dates and floats are not supported (a clear error says so).

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
    Array(Vec<Value>),
    Table(Table),
}

/// A table keeps its keys in file order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Table(pub Vec<(String, Value)>);

impl Table {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Value::Str(s)) => Some(s),
            _ => None,
        }
    }
    pub fn table(&self, key: &str) -> Option<&Table> {
        match self.get(key) {
            Some(Value::Table(t)) => Some(t),
            _ => None,
        }
    }
    fn set(&mut self, key: String, v: Value) -> bool {
        if self.get(&key).is_some() {
            return false;
        }
        self.0.push((key, v));
        true
    }
}

#[derive(Debug, PartialEq)]
pub struct TomlError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for TomlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
    line: usize,
}

type R<T> = Result<T, TomlError>;

impl<'a> P<'a> {
    fn err<T>(&self, m: impl Into<String>) -> R<T> {
        Err(TomlError { line: self.line, message: m.into() })
    }
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn skip_inline_space(&mut self) {
        while matches!(self.peek(), Some(b' ') | Some(b'\t')) {
            self.i += 1;
        }
    }
    fn skip_comment(&mut self) {
        if self.peek() == Some(b'#') {
            while !matches!(self.peek(), None | Some(b'\n')) {
                self.i += 1;
            }
        }
    }
    /// Spaces, comments and line breaks (inside arrays).
    fn skip_all(&mut self) {
        loop {
            self.skip_inline_space();
            self.skip_comment();
            match self.peek() {
                Some(b'\n') => {
                    self.line += 1;
                    self.i += 1;
                }
                Some(b'\r') => self.i += 1,
                _ => return,
            }
        }
    }
    fn key(&mut self) -> R<String> {
        self.skip_inline_space();
        match self.peek() {
            Some(b'"') => self.string(),
            Some(c) if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' => {
                let start = self.i;
                while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_' || c == b'-') {
                    self.i += 1;
                }
                Ok(String::from_utf8_lossy(&self.s[start..self.i]).to_string())
            }
            _ => self.err("expected a key"),
        }
    }
    fn string(&mut self) -> R<String> {
        let quote = self.peek().unwrap();
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            match self.peek() {
                None | Some(b'\n') => return self.err("unterminated string"),
                Some(c) if c == quote => {
                    self.i += 1;
                    break;
                }
                Some(b'\\') if quote == b'"' => {
                    self.i += 1;
                    let e = self.peek();
                    self.i += 1;
                    match e {
                        Some(b'n') => out.push(b'\n'),
                        Some(b't') => out.push(b'\t'),
                        Some(b'r') => out.push(b'\r'),
                        Some(b'"') => out.push(b'"'),
                        Some(b'\\') => out.push(b'\\'),
                        _ => return self.err("unsupported escape in a string"),
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
        String::from_utf8(out).or_else(|_| self.err("a string is not valid UTF-8"))
    }
    fn value(&mut self) -> R<Value> {
        self.skip_inline_space();
        match self.peek() {
            Some(b'"') | Some(b'\'') => self.string().map(Value::Str),
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_all();
                    if self.peek() == Some(b']') {
                        self.i += 1;
                        return Ok(Value::Array(items));
                    }
                    items.push(self.value()?);
                    self.skip_all();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b']') => {}
                        _ => return self.err("expected `,` or `]` in an array"),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut t = Table::default();
                loop {
                    self.skip_inline_space();
                    if self.peek() == Some(b'}') {
                        self.i += 1;
                        return Ok(Value::Table(t));
                    }
                    let k = self.key()?;
                    self.skip_inline_space();
                    if self.peek() != Some(b'=') {
                        return self.err("expected `=` after a key");
                    }
                    self.i += 1;
                    let v = self.value()?;
                    if !t.set(k.clone(), v) {
                        return self.err(format!("the key `{k}` appears twice"));
                    }
                    self.skip_inline_space();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {}
                        _ => return self.err("expected `,` or `}` in an inline table"),
                    }
                }
            }
            Some(b't') if self.s[self.i..].starts_with(b"true") => {
                self.i += 4;
                Ok(Value::Bool(true))
            }
            Some(b'f') if self.s[self.i..].starts_with(b"false") => {
                self.i += 5;
                Ok(Value::Bool(false))
            }
            Some(c) if c.is_ascii_digit() || c == b'-' || c == b'+' => {
                let start = self.i;
                self.i += 1;
                while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'-' || c == b':') {
                    self.i += 1;
                }
                let text = String::from_utf8_lossy(&self.s[start..self.i]).replace('_', "");
                match text.parse::<i64>() {
                    Ok(n) => Ok(Value::Int(n)),
                    Err(_) => self.err(format!("`{text}` is not a supported value (use a string, integer, boolean, array or inline table)")),
                }
            }
            _ => self.err("expected a value"),
        }
    }
}

/// Find (or create) the table at `path` inside `root`; for `[[a]]` the last element of the array.
fn table_at<'t>(root: &'t mut Table, path: &[String], line: usize) -> Result<&'t mut Table, TomlError> {
    let mut cur = root;
    for seg in path {
        let idx = match cur.0.iter().position(|(k, _)| k == seg) {
            Some(i) => i,
            None => {
                cur.0.push((seg.clone(), Value::Table(Table::default())));
                cur.0.len() - 1
            }
        };
        cur = match &mut cur.0[idx].1 {
            Value::Table(t) => t,
            Value::Array(items) => match items.last_mut() {
                Some(Value::Table(t)) => t,
                _ => return Err(TomlError { line, message: format!("`{seg}` is not a table") }),
            },
            _ => return Err(TomlError { line, message: format!("`{seg}` is not a table") }),
        };
    }
    Ok(cur)
}

pub fn parse(text: &str) -> Result<Table, TomlError> {
    let mut p = P { s: text.as_bytes(), i: 0, line: 1 };
    let mut root = Table::default();
    let mut current: Vec<String> = Vec::new();
    loop {
        p.skip_all();
        match p.peek() {
            None => return Ok(root),
            Some(b'[') => {
                let array = p.s.get(p.i + 1) == Some(&b'[');
                p.i += if array { 2 } else { 1 };
                let mut path = Vec::new();
                loop {
                    path.push(p.key()?);
                    p.skip_inline_space();
                    match p.peek() {
                        Some(b'.') => p.i += 1,
                        Some(b']') => {
                            p.i += 1;
                            break;
                        }
                        _ => return p.err("expected `]` after a table name"),
                    }
                }
                if array {
                    if p.peek() != Some(b']') {
                        return p.err("expected `]]`");
                    }
                    p.i += 1;
                    let (last, parents) = path.split_last().unwrap();
                    let parent = table_at(&mut root, parents, p.line)?;
                    match parent.0.iter_mut().find(|(k, _)| k == last) {
                        Some((_, Value::Array(items))) => items.push(Value::Table(Table::default())),
                        Some(_) => return p.err(format!("`{last}` is not an array of tables")),
                        None => parent.0.push((last.clone(), Value::Array(vec![Value::Table(Table::default())]))),
                    }
                } else {
                    // creating it now makes an empty table appear, as in TOML
                    table_at(&mut root, &path, p.line)?;
                }
                current = path;
                p.skip_inline_space();
                p.skip_comment();
                if !matches!(p.peek(), None | Some(b'\n') | Some(b'\r')) {
                    return p.err("unexpected text after a table header");
                }
            }
            Some(_) => {
                let k = p.key()?;
                p.skip_inline_space();
                if p.peek() == Some(b'.') {
                    return p.err("dotted keys are not supported; write a `[table]` or an inline table `{ ... }`");
                }
                if p.peek() != Some(b'=') {
                    return p.err("expected `=` after a key");
                }
                p.i += 1;
                let v = p.value()?;
                let line = p.line;
                let t = table_at(&mut root, &current, line)?;
                if !t.set(k.clone(), v) {
                    return p.err(format!("the key `{k}` appears twice"));
                }
                p.skip_inline_space();
                p.skip_comment();
                if !matches!(p.peek(), None | Some(b'\n') | Some(b'\r')) {
                    return p.err("unexpected text after a value");
                }
            }
        }
    }
}

/// A string as TOML writes it.
pub fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest() {
        let t = parse(
            r#"
# an example
[package]
name = "orbits"   # trailing comment
version = "0.1.0"

[dependencies]
fastmath = { git = "https://example.com/fastmath", tag = "v1.2" }
localutil = { path = "../localutil" }

[native]
link = ["openblas", 'lapack']
"#,
        )
        .unwrap();
        assert_eq!(t.table("package").unwrap().str("name"), Some("orbits"));
        let deps = t.table("dependencies").unwrap();
        assert_eq!(deps.table("fastmath").unwrap().str("tag"), Some("v1.2"));
        assert_eq!(deps.table("localutil").unwrap().str("path"), Some("../localutil"));
        assert_eq!(t.table("native").unwrap().get("link"), Some(&Value::Array(vec![Value::Str("openblas".into()), Value::Str("lapack".into())])));
    }

    #[test]
    fn subtables_arrays_of_tables_and_values() {
        let t = parse("[dependencies.a]\npath = \"x\"\n\n[[package]]\nname = \"a\"\nn = 1_000\nok = true\n\n[[package]]\nname = \"b\"\nlist = [\n  1,\n  2, # two\n]\n").unwrap();
        assert_eq!(t.table("dependencies").unwrap().table("a").unwrap().str("path"), Some("x"));
        let Some(Value::Array(pk)) = t.get("package") else { panic!("{t:?}") };
        assert_eq!(pk.len(), 2);
        let Value::Table(first) = &pk[0] else { panic!() };
        assert_eq!(first.get("n"), Some(&Value::Int(1000)));
        assert_eq!(first.get("ok"), Some(&Value::Bool(true)));
        let Value::Table(second) = &pk[1] else { panic!() };
        assert_eq!(second.get("list"), Some(&Value::Array(vec![Value::Int(1), Value::Int(2)])));
    }

    #[test]
    fn strings_and_escapes() {
        let t = parse("a = \"x\\\"y\\\\z\\n\"\nb = 'C:\\raw'\n").unwrap();
        assert_eq!(t.str("a"), Some("x\"y\\z\n"));
        assert_eq!(t.str("b"), Some("C:\\raw"));
        assert_eq!(parse(&format!("k = {}", quote("a\"b\\c\nd"))).unwrap().str("k"), Some("a\"b\\c\nd"));
    }

    #[test]
    fn errors_have_line_numbers() {
        assert_eq!(parse("a = 1\nb = \n").unwrap_err().line, 2);
        assert_eq!(parse("a = 1\na = 2\n").unwrap_err().line, 2);
        assert!(parse("a.b = 1\n").unwrap_err().message.contains("dotted"));
        assert!(parse("a = 1.5\n").unwrap_err().message.contains("not a supported value"));
        assert!(parse("[a\nb = 1\n").is_err());
        assert!(parse("a = \"unterminated\n").is_err());
        assert!(parse("a = 1 b = 2\n").is_err());
        assert!(parse("[a]\nb = 1\n[a.b]\n").is_err());
    }
}
