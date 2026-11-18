//! Differential mutation fuzzing: take programs from `tests/fuzz_corpus/`, change a number or an
//! operator, and for every mutant that still passes the type and borrow checkers, run it on the
//! interpreter and on the native backend. Whatever the mutation did (overflow, a panic, a
//! different branch), the two must agree on stdout, on whether the program panicked, and on the
//! panic message.
//!
//! `TAU_FUZZ_N` sets the number of mutants (default 30, or 120 in release builds) and `TAU_FUZZ_SEED` the starting seed;
//! the test needs a C compiler and is skipped without one.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tau::interp::Outcome;
use tau::native::{self, NativeError};

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn corpus() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fuzz_corpus");
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) == Some("tau") {
            out.push((p.file_name().unwrap().to_string_lossy().to_string(), std::fs::read_to_string(&p).unwrap()));
        }
    }
    out.sort();
    out
}

/// Byte ranges of decimal integer literals (not part of an identifier or a float).
fn int_literals(src: &str) -> Vec<(usize, usize)> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'.')) {
            let mut j = i;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let float_or_suffixed = j < b.len() && (b[j] == b'.' && j + 1 < b.len() && b[j + 1].is_ascii_digit() || b[j].is_ascii_alphabetic() || b[j] == b'_');
            if !float_or_suffixed {
                out.push((i, j));
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

fn mutate(src: &str, rng: &mut Rng) -> String {
    const NUMS: &[&str] = &["0", "1", "2", "3", "5", "10", "100", "127", "128", "255", "256", "1000", "65535", "2147483647", "9223372036854775807"];
    const OPS: &[(&str, &str)] = &[(" + ", " - "), (" - ", " + "), (" * ", " + "), (" / ", " * "), (" % ", " / "), (" < ", " <= "), (" > ", " >= "), (" == ", " != "), (" && ", " || "), (" <= ", " < ")];
    let mut s = src.to_string();
    let n = 1 + rng.below(2);
    for _ in 0..n {
        if rng.below(3) < 2 {
            let lits = int_literals(&s);
            if lits.is_empty() {
                continue;
            }
            let (a, b) = lits[rng.below(lits.len())];
            s.replace_range(a..b, NUMS[rng.below(NUMS.len())]);
        } else {
            let (from, to) = OPS[rng.below(OPS.len())];
            let spots: Vec<usize> = s.match_indices(from).map(|(i, _)| i).collect();
            if spots.is_empty() {
                continue;
            }
            let at = spots[rng.below(spots.len())];
            s.replace_range(at..at + from.len(), to);
        }
    }
    s
}

/// Run a built native program, killing it if it takes far too long. Its output is read while it
/// runs, so a program that prints a lot cannot block on a full pipe.
fn run_native(src: &str) -> Result<Option<native::NativeOutput>, NativeError> {
    use std::io::Read;
    let exe = native::build_source(src)?;
    let mut child = Command::new(&exe).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| NativeError::Other(e.to_string()))?;
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let out_t = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = so.read_to_end(&mut v);
        v
    });
    let err_t = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = se.read_to_end(&mut v);
        v
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if start.elapsed() > Duration::from_secs(30) => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(None);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(2)),
            Err(e) => return Err(NativeError::Other(e.to_string())),
        }
    };
    Ok(Some(native::NativeOutput {
        stdout: String::from_utf8_lossy(&out_t.join().unwrap_or_default()).into_owned(),
        stderr: String::from_utf8_lossy(&err_t.join().unwrap_or_default()).into_owned(),
        code: status.code().unwrap_or(-1),
    }))
}

#[test]
fn mutated_programs_behave_the_same_natively() {
    if native::find_cc().is_none() {
        eprintln!("skipped: no C compiler");
        return;
    }
    // a short run in debug builds (the interpreter is slow there); `--release` runs a longer one
    let default_n = if cfg!(debug_assertions) { 30 } else { 120 };
    let total: usize = std::env::var("TAU_FUZZ_N").ok().and_then(|v| v.parse().ok()).unwrap_or(default_n);
    let seed: u64 = std::env::var("TAU_FUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x5EED_1234);
    let corpus = corpus();
    let mut rng = Rng(seed | 1);
    let (mut ran, mut rejected, mut skipped) = (0, 0, 0);
    let mut failures = Vec::new();
    let mut tried = 0;
    while ran < total && tried < total * 40 {
        tried += 1;
        let (name, base) = &corpus[rng.below(corpus.len())];
        let src = mutate(base, &mut rng);
        if src == *base {
            continue;
        }
        match tau::analyze_source(&src) {
            Ok(r) if r.diagnostics.is_empty() => {}
            _ => {
                rejected += 1;
                continue;
            }
        }
        if std::env::var("TAU_FUZZ_VERBOSE").is_ok() {
            eprintln!("--- trying a mutant of {name}:\n{src}");
        }
        // the interpreter, with a limit on how long the mutant may run
        let cap = Capture(Arc::new(Mutex::new(Vec::new())));
        let started = Instant::now();
        let r = tau::run_source_limited(&src, Box::new(cap.clone()), 4_000_000).unwrap();
        let interp_secs = started.elapsed().as_secs_f64();
        let want_out = String::from_utf8_lossy(&cap.0.lock().unwrap()).into_owned();
        let want = match r.outcome {
            Some(Outcome::Ok) => None,
            Some(Outcome::Panic(m)) if m == "step limit exceeded" => {
                skipped += 1;
                continue;
            }
            Some(Outcome::Panic(m)) => Some(m),
            _ => {
                skipped += 1;
                continue;
            }
        };
        let got = match run_native(&src) {
            Ok(Some(g)) => g,
            Ok(None) if interp_secs > 5.0 => {
                // a program that is slow on both (an O(n^2) transform of a huge tensor): not a bug
                skipped += 1;
                continue;
            }
            Ok(None) => {
                failures.push(format!("{name}: the native program did not finish in 30 s (the interpreter took {interp_secs:.1} s)\n{src}"));
                continue;
            }
            Err(NativeError::Unsupported(_)) => {
                skipped += 1;
                continue;
            }
            Err(e) => {
                failures.push(format!("{name}: native build failed: {e:?}\n{src}"));
                continue;
            }
        };
        ran += 1;
        let mut problem = None;
        if got.stdout != want_out {
            problem = Some("stdout differs".to_string());
        } else {
            match &want {
                None if got.code != 0 => problem = Some(format!("native exited with {} ({})", got.code, got.stderr.trim())),
                Some(m) if got.code != 101 || got.stderr != format!("panic: {m}\n") => {
                    problem = Some(format!("panic differs: interpreter {m:?}, native exit {} stderr {:?}", got.code, got.stderr))
                }
                _ => {}
            }
        }
        if let Some(p) = problem {
            failures.push(format!("{name}: {p}\n--- interpreter stdout ---\n{want_out}\n--- native stdout ---\n{}\n--- mutant ---\n{src}", got.stdout));
        }
    }
    eprintln!("fuzz: {ran} mutants compared, {rejected} rejected by the checkers, {skipped} skipped (seed {seed})");
    assert!(ran >= total / 2, "too few mutants survived the checkers ({ran} of {total} wanted)");
    assert!(failures.is_empty(), "{} mutant(s) behaved differently:\n\n{}", failures.len(), failures[..failures.len().min(3)].join("\n=====\n"));
}
