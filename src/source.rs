//! Where source text comes from. A program can have many files, but a `Span` is just a pair of
//! offsets, so every file is given its own range of a single offset space (a *base*). The map turns an
//! offset back into a file, a line and a column.

use crate::diag::{snippet, Diagnostic};
use crate::span::Span;

#[derive(Debug)]
pub struct SourceFile {
    /// How the file is shown in messages: a path, or `<std.fs>` for a built-in module.
    pub name: String,
    pub src: String,
    /// The offset of the first byte of this file in the shared offset space.
    pub base: usize,
    /// The file on disk (an absolute path), when there is one.
    pub path: Option<std::path::PathBuf>,
}

#[derive(Default, Debug)]
pub struct SourceMap {
    pub files: Vec<SourceFile>,
    next: usize,
}

impl SourceMap {
    pub fn new() -> SourceMap {
        SourceMap::default()
    }

    /// Add a file; the result is its base offset (the first file gets 0).
    pub fn add(&mut self, name: impl Into<String>, src: String) -> usize {
        let base = self.next;
        // one extra offset, so a span at the very end of a file still belongs to it
        self.next = base + src.len() + 1;
        self.files.push(SourceFile { name: name.into(), src, base, path: None });
        base
    }

    /// The file that contains `offset`.
    pub fn file_at(&self, offset: usize) -> Option<&SourceFile> {
        let i = self.files.partition_point(|f| f.base <= offset);
        i.checked_sub(1).map(|i| &self.files[i])
    }

    /// The file and the span relative to the start of that file.
    pub fn locate(&self, span: Span) -> Option<(&SourceFile, Span)> {
        let f = self.file_at(span.start)?;
        Some((f, Span::new(span.start - f.base, span.end.saturating_sub(f.base).max(span.start - f.base))))
    }

    /// 1-based line and column (in characters) of an offset, with the file's name.
    pub fn line_col(&self, offset: usize) -> Option<(&str, usize, usize)> {
        let f = self.file_at(offset)?;
        let local = (offset - f.base).min(f.src.len());
        let mut at = local;
        while !f.src.is_char_boundary(at) {
            at -= 1;
        }
        let line_start = f.src[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
        Some((&f.name, f.src[..line_start].matches('\n').count() + 1, f.src[line_start..at].chars().count() + 1))
    }

    /// Render a diagnostic whose spans may lie in different files.
    pub fn render(&self, d: &Diagnostic) -> String {
        let mut out = format!("error: {}\n", d.message);
        let one = |out: &mut String, span: Span, label: Option<&str>| match self.locate(span) {
            Some((f, local)) => out.push_str(&snippet(&f.src, &f.name, local, label)),
            None => {
                if let Some(l) = label {
                    out.push_str(&format!("   = note: {l}\n"));
                }
            }
        };
        one(&mut out, d.span, None);
        for (span, note) in &d.notes {
            one(&mut out, *span, Some(note));
        }
        if let Some(h) = &d.hint {
            out.push_str(&format!("   = hint: {h}\n"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_map_back_to_files() {
        let mut m = SourceMap::new();
        let a = m.add("a.tau", "fn a() {}\nfn b() {}\n".to_string());
        let b = m.add("b.tau", "let x\n".to_string());
        assert_eq!(a, 0);
        assert_eq!(b, 21);
        assert_eq!(m.file_at(0).unwrap().name, "a.tau");
        assert_eq!(m.file_at(20).unwrap().name, "a.tau"); // the end of a.tau
        assert_eq!(m.file_at(21).unwrap().name, "b.tau");
        assert_eq!(m.line_col(10), Some(("a.tau", 2, 1)));
        assert_eq!(m.line_col(21 + 4), Some(("b.tau", 1, 5)));
        let d = Diagnostic::new("oops", Span::new(21 + 4, 21 + 5)).with_note(Span::new(3, 4), "see here");
        let text = m.render(&d);
        assert!(text.contains("b.tau:1:5") && text.contains("a.tau:1:4"), "{text}");
    }
}
