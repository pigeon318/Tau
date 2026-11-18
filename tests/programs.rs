//! The programs in `examples/run/` must print exactly what `reference.py` (an
//! independent Python implementation) prints, recorded in the `.expected` files.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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

#[test]
fn programs_match_their_reference_output() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/run");
    let mut checked = 0;
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("tau") {
            continue;
        }
        let expected_path = path.with_extension("expected");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let expected = fs::read_to_string(&expected_path).unwrap_or_else(|_| panic!("{name} has no .expected file"));
        let src = fs::read_to_string(&path).unwrap();
        let cap = Capture(Arc::new(Mutex::new(Vec::new())));
        let r = tau::run_source(&src, Box::new(cap.clone())).unwrap_or_else(|d| panic!("{name}: {}", d.render(&src, &name)));
        assert!(r.diagnostics.is_empty(), "{name} was rejected: {:?}", r.diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>());
        assert_eq!(r.outcome, Some(tau::interp::Outcome::Ok), "{name}");
        let got = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
        assert_eq!(got, expected, "{name}: output differs from the reference");
        checked += 1;
    }
    assert!(checked >= 7, "only {checked} programs were checked");
}

// ---- fuzzing: whatever passes the checkers must run without crashing the interpreter

fn runnable_sources() -> Vec<(String, String)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut out = Vec::new();
    for dir in [root.join("run"), root.join("checked")] {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) == Some("tau") {
                out.push((path.file_name().unwrap().to_string_lossy().to_string(), fs::read_to_string(&path).unwrap()));
            }
        }
    }
    out
}

/// Run a mutant; returns false if the interpreter itself crashed.
fn survives(src: &str) -> (bool, bool) {
    let cap = Capture(Arc::new(Mutex::new(Vec::new())));
    match tau::run_source_limited(src, Box::new(cap), 400_000) {
        Err(_) => (true, false),
        Ok(r) => match r.outcome {
            None => (true, false),
            Some(tau::interp::Outcome::Panic(m)) => (m != "the interpreter crashed", true),
            Some(_) => (true, true),
        },
    }
}

#[test]
fn the_interpreter_survives_mutated_programs() {
    let mut ran = 0;
    for (name, src) in runnable_sources() {
        let lines: Vec<&str> = src.lines().collect();
        let mut variants: Vec<String> = Vec::new();
        for i in 0..lines.len() {
            let mut v = lines.clone();
            v.remove(i);
            variants.push(v.join("\n"));
            let mut v = lines.clone();
            v.insert(i, lines[i]);
            variants.push(v.join("\n"));
            if i + 1 < lines.len() {
                let mut v = lines.clone();
                v.swap(i, i + 1);
                variants.push(v.join("\n"));
            }
        }
        // numeric edits: make loops and indices go out of range, overflow, divide by zero
        for (from, to) in [("0", "1"), ("1", "0"), ("2", "3"), ("+", "-"), ("<", ">"), ("100", "10000000")] {
            let mut start = 0;
            while let Some(pos) = src[start..].find(from) {
                let at = start + pos;
                let mut s = src.clone();
                s.replace_range(at..at + from.len(), to);
                variants.push(s);
                start = at + from.len();
            }
        }
        for v in variants {
            let (alive, did_run) = survives(&v);
            assert!(alive, "the interpreter crashed on a mutation of {name}:\n{v}");
            if did_run {
                ran += 1;
            }
        }
    }
    assert!(ran > 150, "only {ran} mutants ran; the test is not exercising the interpreter");
}
