//! The built-in library: signatures written in Tau (see `src/prelude/*.tau`).
//!
//! `prelude` is global; the others are modules reached with `use`, e.g. `use std.fs`.

use crate::ast::Module;
use crate::parser::parse_module;

/// A parsed source module together with its dotted path (`"std.fs"`).
pub struct ModuleSrc {
    pub path: String,
    pub module: Module,
    /// Part of the compiler's library (its functions may have no body: the runtime provides them).
    pub builtin: bool,
    /// The dotted name of the package this module belongs to, with a trailing dot (`"fastmath."`),
    /// or empty for the program's own package and for built-in modules. A `use` inside the module
    /// is looked up under this prefix first.
    pub package: String,
    /// Which file of the program's `SourceMap` this module was read from (`None` when the module was not
    /// registered in a map).
    pub file: Option<usize>,
}

impl ModuleSrc {
    pub fn as_ref(&self) -> crate::unit::ModRef<'_> {
        crate::unit::ModRef { path: &self.path, module: &self.module, builtin: self.builtin, package: &self.package }
    }
}

const SOURCES: &[(&str, &str)] = &[
    ("prelude", include_str!("prelude/prelude.tau")),
    ("std.fs", include_str!("prelude/std_fs.tau")),
    ("std.io", include_str!("prelude/std_io.tau")),
    ("std.map", include_str!("prelude/std_map.tau")),
    ("std.time", include_str!("prelude/std_time.tau")),
    ("math.linalg", include_str!("prelude/math_linalg.tau")),
    ("math.fft", include_str!("prelude/math_fft.tau")),
];

/// Parse the built-in modules. Panics if one fails to parse: that is a bug in Tau,
/// not in user code.
pub fn load() -> Vec<ModuleSrc> {
    SOURCES
        .iter()
        .map(|(path, src)| match parse_module(src) {
            Ok(module) => ModuleSrc { path: path.to_string(), module, builtin: true, package: String::new(), file: None },
            Err(d) => panic!("built-in module `{path}` failed to parse:\n{}", d.render(src, path)),
        })
        .collect()
}

/// Like `load`, but the modules' sources are added to `map` and their spans lie in the map's
/// offset space, so errors inside them can be told apart from the user's.
pub fn load_into(map: &mut crate::source::SourceMap) -> Vec<ModuleSrc> {
    SOURCES
        .iter()
        .map(|(path, src)| {
            let base = map.add(format!("<{path}>"), src.to_string());
            let file = Some(map.files.len() - 1);
            match crate::parser::parse_module_at(src, base) {
                Ok(module) => ModuleSrc { path: path.to_string(), module, builtin: true, package: String::new(), file },
                Err(d) => panic!("built-in module `{path}` failed to parse:\n{}", map.render(&d)),
            }
        })
        .collect()
}
