//! A whole program: every module (the compiler's library and the program's own files), the entry
//! module, and the source map that tells which file an error is in.
//!
//! Loading from disk (finding the files a program uses, packages and their dependencies) is in
//! `project`; `Workspace::single` is the one-file case used by tests and tools that have only text.

use crate::check::CheckResult;
use crate::diag::Diagnostic;
use crate::prelude::{self, ModuleSrc};
use crate::source::SourceMap;
use crate::unit::ModRef;

pub struct Workspace {
    pub map: SourceMap,
    /// Library modules first (`prelude` is always index 0), then the program's modules.
    pub mods: Vec<ModuleSrc>,
    /// The module that holds `main`.
    pub root: usize,
}

impl Workspace {
    /// A program that is one piece of text, called `name` in messages. Its offsets start at 0, so a
    /// diagnostic can be rendered with `Diagnostic::render(src, name)` as well as with the map.
    pub fn single(name: &str, src: &str) -> Result<Workspace, Diagnostic> {
        let mut map = SourceMap::new();
        let base = map.add(name, src.to_string());
        let module = crate::parser::parse_module_at(src, base)?;
        let mut mods = prelude::load_into(&mut map);
        mods.push(ModuleSrc { path: "main".to_string(), module, builtin: false, package: String::new(), file: Some(0) });
        let root = mods.len() - 1;
        Ok(Workspace { map, mods, root })
    }

    pub fn refs(&self) -> Vec<ModRef<'_>> {
        self.mods.iter().map(|m| m.as_ref()).collect()
    }

    /// Type-check, and if that finds nothing, borrow-check. (Borrow errors are only reported for a
    /// program that type-checks, since they depend on the inferred types.)
    pub fn analyze(&self) -> CheckResult {
        let refs = self.refs();
        let mut r = crate::check::check_modules(&refs, self.root);
        if r.diagnostics.is_empty() {
            r.diagnostics = crate::borrowck::check(&refs, &r.program, &r.results);
        }
        r
    }

    /// An error, with the lines of source it points at (in whichever file they are).
    pub fn render(&self, d: &Diagnostic) -> String {
        self.map.render(d)
    }
}
