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
