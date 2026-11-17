//! The parser must never panic, whatever it is given: only `Ok` or a `Diagnostic`.

use std::fs;
use std::path::PathBuf;

fn example_sources() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
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
    out
}

#[test]
fn truncation_never_panics() {
    for (name, src) in example_sources() {
        for (i, _) in src.char_indices() {
            let cut = &src[..i];
            let r = std::panic::catch_unwind(|| tau::parse_module(cut));
            assert!(r.is_ok(), "panic in {name} truncated at byte {i}");
        }
    }
}

#[test]
fn deleting_one_char_never_panics() {
    for (name, src) in example_sources() {
        let idx: Vec<usize> = src.char_indices().map(|(i, _)| i).collect();
        for &i in &idx {
            let mut s = src.clone();
            s.remove(i);
            let r = std::panic::catch_unwind(|| tau::parse_module(&s));
            assert!(r.is_ok(), "panic in {name} with byte {i} deleted");
        }
    }
}

#[test]
fn inserting_junk_never_panics() {
    let junk = ["(", ")", "{", "}", "[", "]", "\"", "'", ";", "|", "<", ">", "..", "\n", "#", "/*", "@", "mut ", "par "];
    for (name, src) in example_sources().into_iter().take(4) {
        for (i, _) in src.char_indices().step_by(3) {
            for j in junk {
                let mut s = src.clone();
                s.insert_str(i, j);
                let r = std::panic::catch_unwind(|| tau::parse_module(&s));
                assert!(r.is_ok(), "panic in {name} inserting {j:?} at byte {i}");
            }
        }
    }
}

#[test]
fn error_spans_are_valid_and_render() {
    // every error's span must lie inside the source so rendering never panics
    for (name, src) in example_sources() {
        for (i, _) in src.char_indices().step_by(7) {
            let cut = &src[..i];
            if let Err(d) = tau::parse_module(cut) {
                assert!(d.span.start <= cut.len() && d.span.end <= cut.len() + 1,
                    "{name} cut at {i}: span {:?} out of range (len {})", d.span, cut.len());
                let _ = d.render(cut, &name);
            }
        }
    }
}
