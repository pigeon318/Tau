//! Tau: a low-level, math/science-focused language.
//!
//! Stage 1: lexer, parser and AST. Stage 2: name resolution and type checking.
//! Stage 3: the borrow checker. Then an interpreter, so programs can run.
//! See `SPEC.md` for the language design.

pub mod ast;
pub mod borrowck;
pub mod check;
pub mod diag;
pub mod gitdeps;
pub mod editor;
pub mod infer;
pub mod interp;
pub mod lexer;
pub mod native;
pub mod parser;
pub mod prelude;
pub mod project;
pub mod program;
pub mod sexp;
pub mod source;
pub mod span;
pub mod token;
pub mod toml;
pub mod types;
pub mod unit;
pub mod update;
pub mod workspace;

pub use check::{check, check_source, CheckResult};
pub use diag::Diagnostic;
pub use workspace::Workspace;
pub use parser::{parse_expr, parse_module, parse_module_at};

/// Type-check and then borrow-check a module. Borrow errors are only reported for a
/// program that type-checks, since they depend on the inferred types.
pub fn analyze(lib: &[prelude::ModuleSrc], user: &ast::Module) -> CheckResult {
    let mut mods: Vec<unit::ModRef> = lib.iter().map(|m| m.as_ref()).collect();
    mods.push(unit::ModRef { path: "main", module: user, builtin: false, package: "" });
    let root = mods.len() - 1;
    let mut r = check::check_modules(&mods, root);
    if r.diagnostics.is_empty() {
        r.diagnostics = borrowck::check(&mods, &r.program, &r.results);
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
    // the text is parsed on the big stack too: a deeply nested program recurses in the parser
    on_big_stack(|| {
        let ws = Workspace::single("<main>", src)?;
        Ok(run_workspace_here(&ws, out, max_steps))
    })
    .unwrap_or_else(|| Ok(RunResult { diagnostics: Vec::new(), outcome: Some(interp::Outcome::Panic("the interpreter crashed".to_string())) }))
}

/// Run `f` on a thread with a large stack and return its result (`None` if it panicked).
pub fn on_big_stack<T: Send>(f: impl FnOnce() -> T + Send) -> Option<T> {
    std::thread::scope(|s| {
        let h = std::thread::Builder::new().stack_size(STACK_BYTES).spawn_scoped(s, f).expect("cannot start the interpreter thread");
        h.join().ok()
    })
}

/// Check and run an already loaded program (on a big-stack thread).
pub fn run_workspace(ws: &Workspace, out: Box<dyn std::io::Write + Send>, max_steps: u64) -> RunResult {
    on_big_stack(|| run_workspace_here(ws, out, max_steps))
        .unwrap_or(RunResult { diagnostics: Vec::new(), outcome: Some(interp::Outcome::Panic("the interpreter crashed".to_string())) })
}

fn run_workspace_here(ws: &Workspace, out: Box<dyn std::io::Write + Send>, max_steps: u64) -> RunResult {
    let result = ws.analyze();
    if !result.diagnostics.is_empty() {
        return RunResult { diagnostics: result.diagnostics, outcome: None };
    }
    let refs = ws.refs();
    let outcome = interp::run_main(&refs, ws.root, &result, out, STACK_BYTES, max_steps);
    RunResult { diagnostics: Vec::new(), outcome: Some(outcome) }
}

/// The outcome of `test_source`.
pub struct TestReport {
    /// Type or borrow errors. If non-empty no test ran.
    pub diagnostics: Vec<Diagnostic>,
    pub results: Vec<interp::TestResult>,
}

/// Check a program and run its `#[test]` functions (those whose name contains `filter`).
pub fn test_source(src: &str, filter: Option<&str>, max_steps: u64) -> Result<TestReport, Diagnostic> {
    on_big_stack(|| {
        let ws = Workspace::single("<main>", src)?;
        Ok(test_workspace_here(&ws, filter, max_steps))
    })
    .unwrap_or_else(|| {
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

/// Check an already loaded program and run its tests (on a big-stack thread).
pub fn test_workspace(ws: &Workspace, filter: Option<&str>, max_steps: u64) -> TestReport {
    on_big_stack(|| test_workspace_here(ws, filter, max_steps)).unwrap_or(TestReport {
        diagnostics: Vec::new(),
        results: vec![interp::TestResult { name: "<interpreter>".to_string(), passed: false, message: "the interpreter crashed".to_string(), output: String::new() }],
    })
}

fn test_workspace_here(ws: &Workspace, filter: Option<&str>, max_steps: u64) -> TestReport {
    let result = ws.analyze();
    if !result.diagnostics.is_empty() {
        return TestReport { diagnostics: result.diagnostics, results: Vec::new() };
    }
    let refs = ws.refs();
    let results = interp::run_tests(&refs, ws.root, &result, STACK_BYTES, max_steps, filter);
    TestReport { diagnostics: Vec::new(), results }
}
