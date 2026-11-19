//! A program as the compiler stages see it: a list of modules.

use crate::ast::Module;

/// A parsed module with what the stages need to know about it.
#[derive(Clone, Copy)]
pub struct ModRef<'a> {
    /// The dotted path other modules `use` it by (`"std.fs"`, `"util"`, `"util.shapes"`; the entry module is `"main"`).
    pub path: &'a str,
    pub module: &'a Module,
    /// Part of the compiler's library: functions without a body are provided by the runtime.
    pub builtin: bool,
    /// The package prefix (`"fastmath."`) that this module's `use` paths are looked up under first.
    pub package: &'a str,
}
