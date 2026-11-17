//! Every program in `examples/` must parse.

use std::fs;
use std::path::PathBuf;

#[test]
fn all_examples_parse() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut count = 0;
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("tau") {
            continue;
        }
        let src = fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if let Err(d) = tau::parse_module(&src) {
            panic!("{name} failed to parse:\n{}", d.render(&src, &name));
        }
        count += 1;
    }
    assert!(count >= 10, "expected at least 10 examples, found {count}");
}
