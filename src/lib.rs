//! Tau: a low-level, math/science-focused language.
//!
//! Stage 1: lexer, parser and AST. Stage 2: name resolution and type checking.
//! See `SPEC.md` for the language design.

pub mod ast;
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
