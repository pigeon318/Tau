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
    /// No C compiler is installed (the interpreter can still run the program).
    NoCompiler,
    /// Some other problem (no `main`, the C compiler failed).
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
    let ws = crate::Workspace::single("<main>", src).map_err(NativeError::Parse)?;
    emit_c_workspace(&ws)
}

/// Check a whole program (all its modules) and translate it to C.
pub fn emit_c_workspace(ws: &crate::Workspace) -> Result<String, NativeError> {
    let result = ws.analyze();
    if !result.diagnostics.is_empty() {
        return Err(NativeError::Rejected(result.diagnostics));
    }
    // The checker's side tables are keyed by node address, so the modules must stay put.
    let refs = ws.refs();
    let captures = crate::borrowck::capture_modes(&refs, &result.program, &result.results);
    cg::translate(&refs, ws.root, &result, captures)
}

/// The C compiler to use: `$TAU_CC`, else the first of clang, gcc, cc that exists (on Windows:
/// gcc, clang, cc, and only one that can actually build the runtime, since the clang that comes
/// with Visual Studio has no `pthread.h`).
pub fn find_cc() -> Option<String> {
    if let Ok(cc) = std::env::var("TAU_CC") {
        if !cc.is_empty() {
            return Some(cc);
        }
    }
    static FOUND: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    FOUND.get_or_init(find_cc_uncached).clone()
}

fn runs(cc: &str) -> bool {
    Command::new(cc).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

/// Can `cc` compile and link a program that uses what the runtime needs (threads, math)?
fn can_build_runtime(cc: &str) -> bool {
    let dir = cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let id = std::process::id();
    let c = dir.join(format!("probe-{id}.c"));
    let exe = dir.join(format!("probe-{id}{}", std::env::consts::EXE_SUFFIX));
    let ok = std::fs::write(&c, "#include <pthread.h>\n#include <math.h>\n#include <stdint.h>\nint main(void) { __int128 x = 1; return (int)(sqrt(4.0) - 2.0) + (int)x - 1; }\n").is_ok()
        && Command::new(cc).arg(&c).arg("-o").arg(&exe).args(["-lm", "-lpthread"]).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
    let _ = std::fs::remove_file(&c);
    let _ = std::fs::remove_file(&exe);
    ok
}

fn find_cc_uncached() -> Option<String> {
    if cfg!(windows) {
        // remembered per PATH, so the probe runs once, not on every `tau run`
        let mut h = DefaultHasher::new();
        std::env::var("PATH").unwrap_or_default().hash(&mut h);
        let memo = cache_dir().join(format!("cc-{:016x}.txt", h.finish()));
        if let Ok(text) = std::fs::read_to_string(&memo) {
            let name = text.trim();
            return if name.is_empty() || name == "none" { None } else if runs(name) { Some(name.to_string()) } else { None };
        }
        let found = ["gcc", "clang", "cc"].iter().find(|c| runs(c) && can_build_runtime(c)).map(|c| c.to_string());
        let _ = std::fs::create_dir_all(cache_dir());
        let _ = std::fs::write(&memo, found.clone().unwrap_or_else(|| "none".to_string()));
        return found;
    }
    ["clang", "gcc", "cc"].iter().find(|c| runs(c)).map(|c| c.to_string())
}

/// Where compiled programs are cached.
fn cache_dir() -> PathBuf {
    if let Ok(d) = std::env::var("TAU_CACHE") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    if cfg!(windows) {
        if let Ok(d) = std::env::var("LOCALAPPDATA") {
            if !d.is_empty() {
                return Path::new(&d).join("tau").join("cache");
            }
        }
    } else if let Ok(h) = std::env::var("HOME") {
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

/// Extra flags on Windows (MinGW-w64): C99 `printf`/`strtod`, and no dependence on the compiler's DLLs.
#[cfg(windows)]
const PLATFORM_FLAGS: &[&str] = &["-D__USE_MINGW_ANSI_STDIO=1", "-static"];
#[cfg(not(windows))]
const PLATFORM_FLAGS: &[&str] = &[];

/// The file name of a compiled program with this key.
fn exe_name(key: &str) -> String {
    format!("tau-{key}{}", std::env::consts::EXE_SUFFIX)
}

/// Keep the cache from growing without bound: when it holds more than `max` programs, remove the
/// least recently used until `keep` remain.
pub fn prune_cache(dir: &Path, max: usize, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("tau-") && matches!(e.path().extension().and_then(|x| x.to_str()), None | Some("exe")))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if entries.len() <= max {
        return;
    }
    entries.sort();
    let excess = entries.len() - keep;
    for (_, p) in entries.into_iter().take(excess) {
        let _ = std::fs::remove_file(p);
    }
}

/// Delete every cached program; returns how many were removed.
pub fn clean_cache() -> usize {
    let dir = cache_dir();
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.filter_map(|e| e.ok()) {
            if e.file_name().to_string_lossy().starts_with("tau-") && std::fs::remove_file(e.path()).is_ok() {
                n += 1;
            }
        }
    }
    n
}

/// Compile C source to an executable (cached by content); returns its path.
pub fn compile_c(c_src: &str) -> Result<PathBuf, NativeError> {
    let cc = find_cc().ok_or(NativeError::NoCompiler)?;
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| NativeError::plain(format!("cannot create {}: {e}", dir.display())))?;
    // TAU_SANITIZE=1 builds with UBSan and aborts on the first report (used by the test suite's
    // sanitizer run; AddressSanitizer does not work with the runtime's own stack yet)
    let mode = std::env::var("TAU_SANITIZE").unwrap_or_default();
    let extra: &[&str] = match mode.as_str() {
        "" | "0" => &[],
        // data races between the pool's threads
        "thread" => &["-g", "-fsanitize=thread"],
        _ => &["-g", "-fsanitize=undefined", "-fno-sanitize-recover=all"],
    };
    let mut h = DefaultHasher::new();
    (&cc, CFLAGS, extra, c_src).hash(&mut h);
    let key = format!("{:016x}", h.finish());
    let exe = dir.join(exe_name(&key));
    if exe.exists() {
        // remember that it is in use, so pruning removes the ones nobody runs
        if let Ok(f) = std::fs::OpenOptions::new().append(true).open(&exe) {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
        return Ok(exe);
    }
    prune_cache(&dir, 200, 100);
    let c_path = dir.join(format!("tau-{key}-{}.c", std::process::id()));
    std::fs::write(&c_path, c_src).map_err(|e| NativeError::plain(format!("cannot write {}: {e}", c_path.display())))?;
    let tmp_exe = dir.join(format!("tau-{key}-{}.tmp", std::process::id()));
    let out = Command::new(&cc)
        .args(CFLAGS)
        .args(extra)
        .arg(&c_path)
        .args(PLATFORM_FLAGS)
        .arg("-o")
        .arg(&tmp_exe)
        .args(["-lm", "-lpthread"])
        .output()
        .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { NativeError::NoCompiler } else { NativeError::plain(format!("cannot run `{cc}`: {e}")) })?;
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

/// Check, translate and compile a whole program (all its modules); the path of the executable.
pub fn build_workspace(ws: &crate::Workspace) -> Result<PathBuf, NativeError> {
    compile_c(&emit_c_workspace(ws)?)
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
    run_captured_env(src, &[])
}

/// Like `run_captured`, with extra environment variables (e.g. `TAU_THREADS`).
pub fn run_captured_env(src: &str, env: &[(&str, &str)]) -> Result<NativeOutput, NativeError> {
    let exe = build_source(src)?;
    run_exe_captured(&exe, env)
}

/// Build a whole program (all its modules) and run it, capturing its output (used by tests).
pub fn run_workspace_captured(ws: &crate::Workspace) -> Result<NativeOutput, NativeError> {
    let exe = build_workspace(ws)?;
    run_exe_captured(&exe, &[])
}

fn run_exe_captured(exe: &Path, env: &[(&str, &str)]) -> Result<NativeOutput, NativeError> {
    let exe = exe.to_path_buf();
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
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::null());
    let out = retry_busy(|| cmd.output()).map_err(|e| NativeError::plain(format!("cannot run {}: {e}", exe.display())))?;
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

/// Run `f`; a program that was written a moment ago can be reported "text file busy" (Linux error 26)
/// when another thread forks while the file is still open for writing. That passes in a moment, so it
/// is tried again.
fn retry_busy<T>(mut f: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    for _ in 0..100 {
        match f() {
            Err(e) if e.raw_os_error() == Some(26) => std::thread::sleep(std::time::Duration::from_millis(20)),
            r => return r,
        }
    }
    f()
}

/// Run an already built program with the terminal attached; returns its exit code.
pub fn exec(exe: &Path) -> Result<i32, NativeError> {
    let status = retry_busy(|| Command::new(exe).status()).map_err(|e| NativeError::plain(format!("cannot run {}: {e}", exe.display())))?;
    Ok(status.code().unwrap_or(1))
}
