//! The built-in library: signatures written in Tau (see `src/prelude/*.tau`).
//!
//! `prelude` is global; the others are modules reached with `use`, e.g. `use std.fs`.

use crate::ast::Module;
use crate::parser::parse_module;

/// A parsed source module together with its dotted path (`"std.fs"`).
pub struct ModuleSrc {
    pub path: String,
    pub module: Module,
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
            Ok(module) => ModuleSrc {
                path: path.to_string(),
                module,
            },
            Err(d) => panic!("built-in module `{path}` failed to parse:\n{}", d.render(src, path)),
        })
        .collect()
}
