//! Tau: a low-level, math/science-focused language.
//!
//! Stage 1: lexer, parser and AST. Stage 2: name resolution and type checking.
//! Stage 3: the borrow checker.
//! See `SPEC.md` for the language design.

pub mod ast;
pub mod borrowck;
pub mod check;
pub mod diag;
pub mod infer;
pub mod lexer;
pub mod parser;
pub mod prelude;
pub mod program;
pub mod sexp;
pub mod span;
pub mod token;
pub mod types;

pub use check::{check, check_source, CheckResult};
pub use diag::Diagnostic;
pub use parser::{parse_expr, parse_module};

/// Type-check and then borrow-check a module. Borrow errors are only reported for a
/// program that type-checks, since they depend on the inferred types.
pub fn analyze(lib: &[prelude::ModuleSrc], user: &ast::Module) -> CheckResult {
    let mut r = check(lib, user);
    if r.diagnostics.is_empty() {
        r.diagnostics = borrowck::check(user, &r.program, &r.results);
    }
    r
}

/// Parse `src`, then type-check and borrow-check it against the built-in library.
pub fn analyze_source(src: &str) -> Result<CheckResult, Diagnostic> {
    let user = parse_module(src)?;
    let lib = prelude::load();
    Ok(analyze(&lib, &user))
}
