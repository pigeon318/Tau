//! Tau: a low-level, math/science-focused language.
//!
//! Stage 1: lexer, parser and AST. See `SPEC.md` for the language design.

pub mod ast;
pub mod diag;
pub mod lexer;
pub mod parser;
pub mod sexp;
pub mod span;
pub mod token;

pub use diag::Diagnostic;
pub use parser::{parse_expr, parse_module};
