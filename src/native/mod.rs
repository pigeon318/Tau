//! The native backend: translate a checked program to C and compile it with the system C
//! compiler. See `docs/native/DESIGN.md`.
//!
//! The tree-walking interpreter stays the reference implementation; native code must print
//! exactly what the interpreter prints (checked by `tests/native.rs`).

mod cg;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::diag::Diagnostic;

/// The C runtime, embedded so the compiler is a single binary.
pub const RUNTIME: &str = include_str!("runtime.h");

/// Why a native build did not produce a program.
#[derive(Debug)]
pub enum NativeError {
    /// The source does not parse.
    Parse(Diagnostic),
    /// The program has type or borrow errors.
    Rejected(Vec<Diagnostic>),
    /// The program uses something the native backend cannot compile yet.
    Unsupported(String),
    /// Some other problem (no `main`, no C compiler, the C compiler failed).
    Other(String),
}

impl NativeError {
    pub(crate) fn unsupported(msg: impl Into<String>) -> NativeError {
        NativeError::Unsupported(msg.into())
    }
    pub(crate) fn plain(msg: impl Into<String>) -> NativeError {
        NativeError::Other(msg.into())
    }
}

/// Check `src` and translate it to C.
pub fn emit_c(src: &str) -> Result<String, NativeError> {
    let user = crate::parse_module(src).map_err(NativeError::Parse)?;
    let lib = crate::prelude::load();
    let result = crate::analyze(&lib, &user);
    if !result.diagnostics.is_empty() {
        return Err(NativeError::Rejected(result.diagnostics));
    }
    // The checker's side tables are keyed by node address, so `user` must stay put.
    let paths = lib.iter().map(|m| m.path.clone()).collect();
    let captures = crate::borrowck::capture_modes(&user, &result.program, &result.results);
    cg::translate(&user, &result, paths, captures)
}

/// The C compiler to use: `$TAU_CC`, else the first of clang, gcc, cc that exists.
pub fn find_cc() -> Option<String> {
    if let Ok(cc) = std::env::var("TAU_CC") {
        if !cc.is_empty() {
            return Some(cc);
        }
    }
    ["clang", "gcc", "cc"].iter().find(|c| Command::new(c).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)).map(|c| c.to_string())
}

/// Where compiled programs are cached.
fn cache_dir() -> PathBuf {
    if let Ok(d) = std::env::var("TAU_CACHE") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return Path::new(&h).join(".cache").join("tau");
        }
    }
    std::env::temp_dir().join("tau-cache")
}

/// Flags that make native code behave like the interpreter:
///  * `-ffp-contract=off`: no fused multiply-add, so floats round exactly like the interpreter's;
///  * `-fno-optimize-sibling-calls`: infinite recursion overflows the stack (and panics) instead
///    of being quietly turned into a loop;
///  * `-fwrapv`: nothing in generated code relies on signed overflow being undefined.
const CFLAGS: &[&str] = &["-O2", "-std=gnu11", "-ffp-contract=off", "-fno-optimize-sibling-calls", "-fwrapv", "-w"];

/// Compile C source to an executable (cached by content); returns its path.
pub fn compile_c(c_src: &str) -> Result<PathBuf, NativeError> {
    let cc = find_cc().ok_or_else(|| NativeError::plain("no C compiler found: install clang or gcc, or set TAU_CC"))?;
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| NativeError::plain(format!("cannot create {}: {e}", dir.display())))?;
    // TAU_SANITIZE=1 builds with UBSan and aborts on the first report (used by the test suite's
    // sanitizer run; AddressSanitizer does not work with the runtime's own stack yet)
    let sanitize = std::env::var("TAU_SANITIZE").map(|v| !v.is_empty() && v != "0").unwrap_or(false);
    let extra: &[&str] = if sanitize { &["-g", "-fsanitize=undefined", "-fno-sanitize-recover=all"] } else { &[] };
    let mut h = DefaultHasher::new();
    (&cc, CFLAGS, extra, c_src).hash(&mut h);
    let key = format!("{:016x}", h.finish());
    let exe = dir.join(format!("tau-{key}"));
    if exe.exists() {
        return Ok(exe);
    }
    let c_path = dir.join(format!("tau-{key}-{}.c", std::process::id()));
    std::fs::write(&c_path, c_src).map_err(|e| NativeError::plain(format!("cannot write {}: {e}", c_path.display())))?;
    let tmp_exe = dir.join(format!("tau-{key}-{}.tmp", std::process::id()));
    let out = Command::new(&cc)
        .args(CFLAGS)
        .args(extra)
        .arg(&c_path)
        .arg("-o")
        .arg(&tmp_exe)
        .args(["-lm", "-lpthread"])
        .output()
        .map_err(|e| NativeError::plain(format!("cannot run `{cc}`: {e}")))?;
    let _ = std::fs::remove_file(&c_path);
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp_exe);
        return Err(NativeError::plain(format!(
            "the C compiler rejected the generated code (this is a bug in Tau):\n{}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    std::fs::rename(&tmp_exe, &exe).map_err(|e| NativeError::plain(format!("cannot install {}: {e}", exe.display())))?;
    Ok(exe)
}

/// Check, translate and compile `src`; returns the executable.
pub fn build_source(src: &str) -> Result<PathBuf, NativeError> {
    compile_c(&emit_c(src)?)
}

/// What a native program did.
#[derive(Debug, PartialEq)]
pub struct NativeOutput {
    pub stdout: String,
    pub stderr: String,
    /// `0` for a normal exit, `101` for a panic.
    pub code: i32,
}

/// Build `src` and run it, capturing its output (used by tests).
pub fn run_captured(src: &str) -> Result<NativeOutput, NativeError> {
    let exe = build_source(src)?;
    // TAU_VALGRIND=1 runs the program under valgrind: an invalid access or double free gives exit
    // status 99, and a leak on a normal exit gives 98 (used by the test suite's memory run)
    let valgrind = std::env::var("TAU_VALGRIND").map(|v| !v.is_empty() && v != "0").unwrap_or(false);
    let log = std::env::temp_dir().join(format!("tau-valgrind-{}-{:x}.log", std::process::id(), {
        let mut h = DefaultHasher::new();
        exe.hash(&mut h);
        std::thread::current().id().hash(&mut h);
        h.finish()
    }));
    let mut cmd = if valgrind {
        let mut c = Command::new("valgrind");
        c.args(["--leak-check=full", "--show-leak-kinds=definite", "--errors-for-leak-kinds=none", "--error-exitcode=99", "-q"]).arg(format!("--log-file={}", log.display())).arg(&exe);
        c
    } else {
        Command::new(&exe)
    };
    let out = cmd.stdin(Stdio::null()).output().map_err(|e| NativeError::plain(format!("cannot run {}: {e}", exe.display())))?;
    let mut stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let mut code = out.status.code().unwrap_or(-1);
    if valgrind {
        let report = std::fs::read_to_string(&log).unwrap_or_default();
        let _ = std::fs::remove_file(&log);
        if code == 99 {
            // an invalid access or double free
            stderr.push_str(&report);
        } else if code == 0 && !report.trim().is_empty() {
            // a normal exit that leaked: a program that panics is not expected to free anything
            code = 98;
            stderr.push_str(&report);
        }
    }
    Ok(NativeOutput { stdout: String::from_utf8_lossy(&out.stdout).into_owned(), stderr, code })
}

/// Run an already built program with the terminal attached; returns its exit code.
pub fn exec(exe: &Path) -> Result<i32, NativeError> {
    let status = Command::new(exe).status().map_err(|e| NativeError::plain(format!("cannot run {}: {e}", exe.display())))?;
    Ok(status.code().unwrap_or(1))
}
