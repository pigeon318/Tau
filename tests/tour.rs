//! Every program in `examples/tour/` must check, run on the interpreter and natively, and give the same output.
//! (`speed.tau` prints timings, so it is only checked.)

use std::fs;
use std::path::PathBuf;

fn tour() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples").join("tour");
    let mut out: Vec<(String, String)> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("tau"))
        .map(|p| (p.file_name().unwrap().to_string_lossy().to_string(), fs::read_to_string(&p).unwrap()))
        .collect();
    out.sort();
    out
}

#[test]
fn the_tour_programs_check() {
    let programs = tour();
    assert!(programs.len() >= 10, "the tour is missing programs");
    for (name, src) in programs {
        let r = tau::check_source(&src).unwrap_or_else(|d| panic!("{name} does not parse: {}", d.message));
        assert!(r.diagnostics.is_empty(), "{name}: {}", r.diagnostics.iter().map(|d| d.message.clone()).collect::<Vec<_>>().join("; "));
    }
}

#[test]
fn the_tour_programs_give_the_same_output_on_both_backends() {
    if std::process::Command::new(std::env::var("TAU_CC").unwrap_or_else(|_| "cc".to_string())).arg("--version").output().is_err() {
        eprintln!("skipped: no C compiler");
        return;
    }
    let exe = env!("CARGO_BIN_EXE_tau");
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for (name, _) in tour() {
        if name == "speed.tau" {
            continue;
        }
        let path = dir.join("examples").join("tour").join(&name);
        let run = |flag: &str| {
            let out = std::process::Command::new(exe).arg("run").arg(&path).arg(flag).output().unwrap();
            assert!(out.status.success(), "{name} {flag}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        let (a, b) = (run("--interp"), run("--native"));
        assert!(!a.is_empty(), "{name} printed nothing");
        assert_eq!(a, b, "{name}: the interpreter and native code differ");
    }
}
