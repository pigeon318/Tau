//! Every program in `examples/checked/` must type-check cleanly, and no binding
//! may silently end up with the error type.

use std::fs;
use std::path::PathBuf;

fn checked_examples() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/checked");
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("tau") {
            out.push((
                path.file_name().unwrap().to_string_lossy().to_string(),
                fs::read_to_string(&path).unwrap(),
            ));
        }
    }
    out.sort();
    out
}

#[test]
fn checked_examples_have_no_errors() {
    let examples = checked_examples();
    assert!(examples.len() >= 7, "expected the checked examples, found {}", examples.len());
    for (name, src) in examples {
        let r = tau::check_source(&src).unwrap_or_else(|d| panic!("{name}: {}", d.render(&src, &name)));
        let errs: Vec<String> = r.diagnostics.iter().map(|d| d.render(&src, &name)).collect();
        assert!(errs.is_empty(), "{name} has type errors:\n{}", errs.join("\n"));
        let bad = |t: &tau::types::Ty| {
            let d = format!("{t:?}");
            d.contains("Var(") || d.contains("Error")
        };
        for b in &r.results.bindings {
            assert!(
                !bad(&b.ty),
                "{name}: binding `{}` has an unresolved or error type `{}`",
                b.name,
                r.show(&b.ty)
            );
        }
        for (k, t) in &r.results.expr_ty {
            assert!(!bad(t), "{name}: an expression (node {k:?}) has an unresolved or error type");
        }
    }
}

#[test]
fn the_empty_program_checks_so_the_prelude_is_sound() {
    let r = tau::check_source("").unwrap();
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
}
