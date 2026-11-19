//! The JSON that `tau check --json` prints for editors.

use tau::editor::check_json;

fn contains(json: &str, needle: &str) -> bool {
    json.contains(needle)
}

#[test]
fn a_good_program_has_no_diagnostics_and_has_hovers() {
    let j = check_json("fn add(a: i32, b: i32) -> i32 { a + b }\nfn main() {\n    let x = add(1, 2)\n    print(x)\n}\n");
    assert!(contains(&j, "\"ok\":true"), "{j}");
    assert!(contains(&j, "\"diagnostics\":[]"), "{j}");
    assert!(contains(&j, "fn add(a: i32, b: i32) -> i32"), "{j}");
    assert!(contains(&j, "x: i32"), "{j}");
}

#[test]
fn type_errors_have_zero_based_ranges() {
    let j = check_json("fn main() {\n    let x: i32 = \"a\"\n}\n");
    assert!(contains(&j, "\"ok\":false"), "{j}");
    assert!(contains(&j, "\"line\":1"), "{j}");
    assert!(contains(&j, "\"character\":17"), "{j}");
}

#[test]
fn columns_count_utf16_units() {
    // "é" is one unit (two bytes) and the emoji two units (four bytes), so the string "a"
    // starts at unit 21, not at byte 25 or at character 20
    let j = check_json("fn main() {\n    print(\"é😀\", 1 + \"a\")\n}\n");
    assert!(contains(&j, "\"start\":{\"line\":1,\"character\":21}"), "{j}");
}

#[test]
fn parse_errors_are_reported_too() {
    let j = check_json("fn main( {\n");
    assert!(contains(&j, "\"ok\":false"), "{j}");
    assert!(contains(&j, "\"hovers\":[]"), "{j}");
}

#[test]
fn json_is_escaped() {
    let j = check_json("fn main() { let x = \"a\\\"b\" + 1 }\n");
    // whatever the message is, the output must be one valid JSON line
    assert!(!j.contains('\n'), "{j}");
}

// ------------------------------------------------------------------ files that are part of a package

mod files {
    use std::path::{Path, PathBuf};
    use tau::editor::check_json_file;
    use tau::update::{parse_json, Json};

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(files: &[(&str, &str)]) -> Tmp {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!("tau-editor-test-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
            std::fs::create_dir_all(&dir).unwrap();
            let t = Tmp(dir.canonicalize().unwrap());
            for (p, text) in files {
                let f = t.0.join(p);
                std::fs::create_dir_all(f.parent().unwrap()).unwrap();
                std::fs::write(f, text).unwrap();
            }
            t
        }
        fn path(&self, p: &str) -> PathBuf {
            self.0.join(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn files_of(j: &Json) -> Vec<(String, String)> {
        j.get("diagnostics")
            .and_then(Json::as_arr)
            .unwrap()
            .iter()
            .map(|d| (d.get("file").and_then(Json::as_str).unwrap_or("").to_string(), d.get("message").and_then(Json::as_str).unwrap().to_string()))
            .collect()
    }

    fn check(path: &Path, text: &str) -> Json {
        parse_json(&check_json_file(path, text)).expect("valid JSON")
    }

    const PROJECT: &[(&str, &str)] = &[
        ("tau.toml", "[package]\nname = \"p\"\n"),
        ("src/main.tau", "use util\n\nfn main() {\n    let x = util.double(21)\n    print(x)\n}\n"),
        ("src/util.tau", "pub fn double(x: i64) -> i64 { x * 2 }\n"),
        ("src/orphan.tau", "pub fn alone() -> i64 { 1 }\n"),
    ];

    #[test]
    fn a_file_that_uses_another_file_has_no_false_errors_and_has_hovers() {
        let t = Tmp::new(PROJECT);
        let main = t.path("src/main.tau");
        let text = std::fs::read_to_string(&main).unwrap();
        let j = check(&main, &text);
        assert_eq!(j.get("ok"), Some(&Json::Bool(true)), "{j:?}");
        let hovers = j.get("hovers").and_then(Json::as_arr).unwrap();
        let texts: Vec<&str> = hovers.iter().filter_map(|h| h.get("text").and_then(Json::as_str)).collect();
        assert!(texts.contains(&"x: i64"), "{texts:?}");
    }

    #[test]
    fn errors_come_with_the_file_they_are_in() {
        let t = Tmp::new(PROJECT);
        // the unsaved text of main.tau has an error in itself
        let main = t.path("src/main.tau");
        let j = check(&main, "use util\nfn main() {\n    let x: str = util.double(1)\n}\n");
        let found = files_of(&j);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, tau::project::display_path(&main));
        assert!(found[0].1.contains("mismatched types"));
        // the file on disk has an error, and another file is the one being edited
        let util = t.path("src/util.tau");
        std::fs::write(&util, "pub fn double(x: i64) -> i64 { \"no\" }\n").unwrap();
        let j = check(&main, &std::fs::read_to_string(&main).unwrap());
        let found = files_of(&j);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, tau::project::display_path(&util));
    }

    #[test]
    fn unsaved_text_replaces_what_is_on_disk() {
        let t = Tmp::new(PROJECT);
        let util = t.path("src/util.tau");
        // the buffer renames the function: main.tau (on disk) now fails, util.tau (the buffer) is fine
        let j = check(&util, "pub fn twice(x: i64) -> i64 { x * 2 }\n");
        let found = files_of(&j);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, tau::project::display_path(&t.path("src/main.tau")));
        assert!(found[0].1.contains("has no function `double`"), "{found:?}");
    }

    #[test]
    fn files_that_nothing_imports_are_checked_too() {
        let t = Tmp::new(PROJECT);
        let orphan = t.path("src/orphan.tau");
        let j = check(&orphan, "pub fn alone() -> i64 { \"x\" }\n");
        let found = files_of(&j);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, tau::project::display_path(&orphan));
        // its hovers are reported too
        let j = check(&orphan, "pub fn alone(a: i64) -> i64 { a }\n");
        let hovers = j.get("hovers").and_then(Json::as_arr).unwrap();
        assert!(hovers.iter().any(|h| h.get("text").and_then(Json::as_str) == Some("a: i64")), "{hovers:?}");
    }

    #[test]
    fn syntax_errors_in_any_file_are_found() {
        let t = Tmp::new(PROJECT);
        let main = t.path("src/main.tau");
        let j = check(&main, "use util\nfn main( {\n");
        let found = files_of(&j);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, tau::project::display_path(&main));
        std::fs::write(t.path("src/util.tau"), "pub fn f( {\n").unwrap();
        let j = check(&main, &std::fs::read_to_string(&main).unwrap());
        let found = files_of(&j);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, tau::project::display_path(&t.path("src/util.tau")));
    }

    #[test]
    fn a_single_file_outside_a_package_and_a_file_not_on_disk() {
        let t = Tmp::new(&[("loose.tau", "use sibling\nfn main() {\n    print(sibling.f())\n}\n"), ("sibling.tau", "pub fn f() -> i64 { 1 }\n")]);
        let loose = t.path("loose.tau");
        let j = check(&loose, &std::fs::read_to_string(&loose).unwrap());
        assert_eq!(j.get("ok"), Some(&Json::Bool(true)), "{j:?}");
        // not saved yet: still gives an answer for the text
        let j = check(&t.path("new.tau"), "fn main() {\n    let x: i64 = \"a\"\n}\n");
        assert_eq!(j.get("ok"), Some(&Json::Bool(false)));
        assert_eq!(files_of(&j).len(), 1);
    }

    #[test]
    fn a_broken_manifest_is_reported_on_the_file() {
        let t = Tmp::new(&[("tau.toml", "[package]\n"), ("src/main.tau", "fn main() {}\n")]);
        let main = t.path("src/main.tau");
        let j = check(&main, "fn main() {}\n");
        let found = files_of(&j);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].1.contains("tau.toml"), "{found:?}");
        assert_eq!(found[0].0, tau::project::display_path(&main));
    }

    #[test]
    fn the_command_line_takes_the_file_and_the_text() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let t = Tmp::new(PROJECT);
        let main = t.path("src/main.tau");
        let mut c = Command::new(env!("CARGO_BIN_EXE_tau")).args(["check", "-", "--json", "--file"]).arg(&main).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        c.stdin.take().unwrap().write_all(b"use util\nfn main() {\n    print(util.double(1))\n}\n").unwrap();
        let o = c.wait_with_output().unwrap();
        let j = parse_json(String::from_utf8_lossy(&o.stdout).trim()).unwrap();
        assert_eq!(j.get("ok"), Some(&Json::Bool(true)), "{j:?}");
    }
}
