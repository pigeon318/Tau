//! Tau: a low-level, math/science-focused language.
//!
//! Stage 1: lexer, parser and AST. Stage 2: name resolution and type checking.
//! Stage 3: the borrow checker. Then an interpreter, so programs can run.
//! See `SPEC.md` for the language design.

pub mod ast;
pub mod borrowck;
pub mod check;
pub mod diag;
pub mod infer;
pub mod interp;
pub mod lexer;
pub mod native;
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

/// Stack for the interpreter thread (address space is reserved lazily).
const STACK_BYTES: usize = 1 << 30;

/// The result of `run_source`: either the program was rejected, or it ran.
pub struct RunResult {
    /// Type or borrow errors. If non-empty the program did not run.
    pub diagnostics: Vec<Diagnostic>,
    pub outcome: Option<interp::Outcome>,
}

/// Check and then run a program, writing its output to `out`. The interpreter runs on
/// a thread with a large stack, because evaluating deeply recursive programs recurses
/// in the host too.
pub fn run_source(src: &str, out: Box<dyn std::io::Write + Send>) -> Result<RunResult, Diagnostic> {
    run_source_limited(src, out, u64::MAX)
}

/// Like `run_source`, but the program is stopped with a panic after `max_steps`
/// expressions have been evaluated (used to test programs that might not terminate).
pub fn run_source_limited(src: &str, out: Box<dyn std::io::Write + Send>, max_steps: u64) -> Result<RunResult, Diagnostic> {
    let src = src.to_string();
    let worker = std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || -> Result<RunResult, Diagnostic> {
            let user = parse_module(&src)?;
            let lib = prelude::load();
            let result = analyze(&lib, &user);
            if !result.diagnostics.is_empty() {
                return Ok(RunResult { diagnostics: result.diagnostics, outcome: None });
            }
            let paths = lib.iter().map(|m| m.path.clone()).collect();
            let outcome = interp::run_main(&user, &result, paths, out, STACK_BYTES, max_steps);
            Ok(RunResult { diagnostics: Vec::new(), outcome: Some(outcome) })
        })
        .expect("cannot start the interpreter thread");
    worker.join().unwrap_or_else(|_| {
        Ok(RunResult { diagnostics: Vec::new(), outcome: Some(interp::Outcome::Panic("the interpreter crashed".to_string())) })
    })
}

/// The outcome of `test_source`.
pub struct TestReport {
    /// Type or borrow errors. If non-empty no test ran.
    pub diagnostics: Vec<Diagnostic>,
    pub results: Vec<interp::TestResult>,
}

/// Check a program and run its `#[test]` functions (those whose name contains `filter`).
pub fn test_source(src: &str, filter: Option<&str>, max_steps: u64) -> Result<TestReport, Diagnostic> {
    let src = src.to_string();
    let filter = filter.map(|s| s.to_string());
    let worker = std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || -> Result<TestReport, Diagnostic> {
            let user = parse_module(&src)?;
            let lib = prelude::load();
            let result = analyze(&lib, &user);
            if !result.diagnostics.is_empty() {
                return Ok(TestReport { diagnostics: result.diagnostics, results: Vec::new() });
            }
            let paths = lib.iter().map(|m| m.path.clone()).collect();
            let results = interp::run_tests(&user, &result, paths, STACK_BYTES, max_steps, filter.as_deref());
            Ok(TestReport { diagnostics: Vec::new(), results })
        })
        .expect("cannot start the test thread");
    worker.join().unwrap_or_else(|_| {
        Ok(TestReport {
            diagnostics: Vec::new(),
            results: vec![interp::TestResult {
                name: "<interpreter>".to_string(),
                passed: false,
                message: "the interpreter crashed".to_string(),
                output: String::new(),
            }],
        })
    })
}
