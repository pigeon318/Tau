//! Every program in `examples/borrow_errors/` must be rejected with exactly the error
//! named on its first line (`// error: ...`); every program in `examples/checked/`
//! must pass both the type checker and the borrow checker.

use std::fs;
use std::path::PathBuf;

fn programs(dir: &str) -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(dir);
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("tau") {
            out.push((path.file_name().unwrap().to_string_lossy().to_string(), fs::read_to_string(&path).unwrap()));
        }
    }
    out.sort();
    out
}

#[test]
fn each_borrow_error_example_is_rejected_for_the_stated_reason() {
    let examples = programs("examples/borrow_errors");
    assert!(examples.len() >= 10, "found only {} borrow-error examples", examples.len());
    for (name, src) in examples {
        let want = src
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("// error: "))
            .unwrap_or_else(|| panic!("{name}: the first line must be `// error: <message>`"));
        let r = tau::analyze_source(&src).unwrap_or_else(|d| panic!("{name}: {}", d.render(&src, &name)));
        let msgs: Vec<&str> = r.diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(msgs.len(), 1, "{name}: expected exactly one error, got {msgs:?}");
        assert!(msgs[0].contains(want), "{name}: expected {want:?}, got {:?}", msgs[0]);
    }
}

#[test]
fn checked_examples_pass_the_borrow_checker_too() {
    for (name, src) in programs("examples/checked") {
        let r = tau::analyze_source(&src).unwrap_or_else(|d| panic!("{name}: {}", d.render(&src, &name)));
        let errs: Vec<String> = r.diagnostics.iter().map(|d| d.render(&src, &name)).collect();
        assert!(errs.is_empty(), "{name} was rejected:\n{}", errs.join("\n"));
    }
}
