//! Error messages: they should say what is wrong and where.

use tau::parse_module;

fn err(src: &str) -> String {
    match parse_module(src) {
        Ok(_) => panic!("expected an error for {src:?}"),
        Err(d) => d.render(src, "t.tau"),
    }
}

#[test]
fn semicolons_are_rejected_with_a_hint() {
    let out = err("fn main() {\n    let x = 1;\n}");
    assert!(out.contains("doesn't use semicolons"), "{out}");
    assert!(out.contains("2:14"), "{out}");
    assert!(out.contains("hint"), "{out}");
}

#[test]
fn unclosed_brace_points_at_the_opener() {
    let out = err("fn main() {\n    let x = 1\n");
    assert!(out.contains("unclosed `{`"), "{out}");
    assert!(out.contains("1:11"), "{out}");
}

#[test]
fn mismatched_delimiters() {
    let out = err("fn main() { foo(1, 2} }");
    assert!(out.contains("mismatched closing `}`, expected `)`"), "{out}");
}

#[test]
fn expected_token_message() {
    let out = err("fn (a) {}");
    assert!(out.contains("expected a function name, found `(`"), "{out}");
}

#[test]
fn missing_newline_between_statements() {
    let out = err("fn main() { let a = 1 let b = 2 }");
    assert!(out.contains("new line or `}` after this statement"), "{out}");
}

#[test]
fn mut_on_the_name_gets_a_hint() {
    let out = err("fn f(mut v: [f64]) {}");
    assert!(out.contains("name: mut Type"), "{out}");
}

#[test]
fn own_and_mut_conflict() {
    let out = err("fn f(own v: mut [f64]) {}");
    assert!(out.contains("both `own` and `mut`"), "{out}");
}

#[test]
fn par_needs_for_or_reduction() {
    let out = err("fn f() { par 5 }");
    assert!(out.contains("after `par`"), "{out}");
}

#[test]
fn unterminated_string() {
    let out = err("fn f() { print(\"hi) }");
    assert!(out.contains("unterminated string"), "{out}");
}

#[test]
fn errors_inside_interpolations_point_at_the_right_column() {
    // the stray `)` is at column 21 of line 1
    let src = "fn f() { print(\"x {1 + )}\") }";
    let out = err(src);
    let col = src.find(")}").unwrap() + 1;
    assert!(out.contains(&format!("1:{col}")), "wanted column {col}:\n{out}");
}

#[test]
fn not_an_item() {
    let out = err("let x = 1");
    assert!(out.contains("expected an item"), "{out}");
}

#[test]
fn caret_underlines_the_token() {
    let out = err("fn f() {\n    foo(1, 2}\n}");
    assert!(out.contains("^"), "{out}");
}

#[test]
fn unit_literals_are_not_supported_yet() {
    // `9.8 m/s` is reserved for post-v1, so it is a clean error for now
    let out = err("fn f() { let v = 9.8 m }");
    assert!(out.contains("new line or `}`"), "{out}");
}
