//! Benchmarks as tests: each program in `examples/bench/` runs on the interpreter and natively;
//! the outputs must match and the native build must be much faster (except the tensor kernels,
//! which the interpreter already runs as compiled code).
//!
//! These take about a minute and are ignored by default. Run them in release mode:
//!
//!     cargo test --release --test native_bench -- --ignored --nocapture

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tau::native;

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
#[ignore]
fn native_programs_are_much_faster_than_the_interpreter() {
    if native::find_cc().is_none() {
        eprintln!("skipped: no C compiler");
        return;
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples").join("bench");
    let mut names: Vec<String> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().and_then(|x| x.to_str()) == Some("tau")).map(|p| p.file_stem().unwrap().to_string_lossy().to_string()).collect();
    names.sort();
    println!("\n{:<12} {:>10} {:>10} {:>8}", "program", "interp", "native", "speedup");
    for name in names {
        let src = std::fs::read_to_string(dir.join(format!("{name}.tau"))).unwrap();
        // interpreter
        let cap = Capture(Arc::new(Mutex::new(Vec::new())));
        let t = Instant::now();
        let r = tau::run_source(&src, Box::new(cap.clone())).unwrap();
        let interp = t.elapsed().as_secs_f64();
        assert!(r.diagnostics.is_empty(), "{name} was rejected");
        let want = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
        // native: build first (not timed), then run
        let exe = native::build_source(&src).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let t = Instant::now();
        let out = std::process::Command::new(&exe).output().unwrap();
        let nat = t.elapsed().as_secs_f64();
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}: output differs");
        let speedup = interp / nat.max(1e-6);
        println!("{name:<12} {interp:>9.2}s {nat:>9.3}s {speedup:>7.0}x");
        assert!(nat < 10.0, "{name} took {nat} s natively");
        // tensor kernels are already compiled Rust in the interpreter: no speed-up is expected there
        if name != "matmul" {
            assert!(speedup > 5.0, "{name}: native is only {speedup:.1}x faster");
        }
    }
}
