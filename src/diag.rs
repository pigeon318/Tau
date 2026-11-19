//! Compiler diagnostics (errors) and their rendering.

use crate::span::Span;

#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub message: String,
    pub span: Span,
    pub hint: Option<String>,
    /// Secondary locations, each shown with its own source line ("value moved here").
    pub notes: Vec<(Span, String)>,
}

impl Diagnostic {
    pub fn new(message: impl Into<String>, span: Span) -> Diagnostic {
        Diagnostic {
            message: message.into(),
            span,
            hint: None,
            notes: Vec::new(),
        }
    }

    pub fn with_note(mut self, span: Span, note: impl Into<String>) -> Diagnostic {
        self.notes.push((span, note.into()));
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Diagnostic {
        self.hint = Some(hint.into());
        self
    }

    /// Render the diagnostic with the offending source line and a caret underline.
    ///
    /// ```text
    /// error: expected `)`, found `}`
    ///  --> main.tau:3:12
    ///   |
    /// 3 |     foo(1, 2}
    ///   |            ^
    ///   = hint: ...
    /// ```
    pub fn render(&self, src: &str, filename: &str) -> String {
        let mut out = format!("error: {}\n", self.message);
        out.push_str(&snippet(src, filename, self.span, None));
        for (span, note) in &self.notes {
            out.push_str(&snippet(src, filename, *span, Some(note)));
        }
        if let Some(h) = &self.hint {
            out.push_str(&format!("   = hint: {h}\n"));
        }
        out
    }
}

/// A source line with a caret underline; `label` is printed after the carets.
pub(crate) fn snippet(src: &str, filename: &str, span: Span, label: Option<&str>) -> String {
    let mut start = span.start.min(src.len());
    while !src.is_char_boundary(start) {
        start -= 1;
    }
    let line_start = src[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = src[start..].find('\n').map(|i| start + i).unwrap_or(src.len());
    let line_no = src[..line_start].matches('\n').count() + 1;
    let line_text = src[line_start..line_end].trim_end_matches('\r');
    let col = src[line_start..start].chars().count();

    let mut end = span.end.min(line_end).max(start);
    while !src.is_char_boundary(end) {
        end -= 1;
    }
    let width = src[start..end].chars().count().max(1);

    let num = line_no.to_string();
    let pad = " ".repeat(num.len());
    let mut out = String::new();
    out.push_str(&format!("{pad}--> {filename}:{line_no}:{}\n", col + 1));
    out.push_str(&format!("{pad} |\n"));
    out.push_str(&format!("{num} | {line_text}\n"));
    let carets = "^".repeat(width);
    match label {
        Some(l) => out.push_str(&format!("{pad} | {}{carets} {l}\n", " ".repeat(col))),
        None => out.push_str(&format!("{pad} | {}{carets}\n", " ".repeat(col))),
    }
    out
}
