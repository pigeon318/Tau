//! `tau test`: discovering and running `#[test]` functions.

use tau::{test_source, TestReport};

fn run(src: &str, filter: Option<&str>) -> TestReport {
    match test_source(src, filter, 5_000_000) {
        Ok(r) => r,
        Err(d) => panic!("{}", d.render(src, "<test>")),
    }
}

fn outcomes(src: &str) -> Vec<(String, bool)> {
    let r = run(src, None);
    assert!(
        r.diagnostics.is_empty(),
        "rejected:\n{}",
        r.diagnostics.iter().map(|d| d.render(src, "<test>")).collect::<Vec<_>>().join("\n")
    );
    r.results.into_iter().map(|t| (t.name, t.passed)).collect()
}

#[test]
fn passing_and_failing_tests_are_reported_separately() {
    let src = "#[test]\nfn good() { assert_eq(1 + 1, 2) }\n#[test]\nfn bad() { assert_eq(1 + 1, 3) }\n#[test]\nfn also_good() { assert(true) }";
    assert_eq!(outcomes(src), vec![("good".into(), true), ("bad".into(), false), ("also_good".into(), true)]);
}

#[test]
fn a_failure_does_not_stop_later_tests() {
    let src = "#[test]\nfn first() { panic(\"boom\") }\n#[test]\nfn second() { assert(true) }";
    assert_eq!(outcomes(src), vec![("first".into(), false), ("second".into(), true)]);
}

#[test]
fn the_failure_message_and_output_are_captured() {
    let src = "#[test]\nfn noisy() {\n print(\"about to fail\")\n assert_eq(2, 3)\n}";
    let r = run(src, None);
    let t = &r.results[0];
    assert!(!t.passed);
    assert_eq!(t.output, "about to fail\n");
    assert!(t.message.contains("left != right"), "{}", t.message);
}

#[test]
fn a_passing_test_keeps_its_output_quiet() {
    let r = run("#[test]\nfn fine() { print(\"hello\") }", None);
    assert!(r.results[0].passed);
    assert_eq!(r.results[0].output, "hello\n");
}

#[test]
fn should_panic() {
    let src = "#[test]\n#[should_panic]\nfn does() { panic(\"x\") }\n#[test]\n#[should_panic]\nfn does_not() { assert(true) }";
    assert_eq!(outcomes(src), vec![("does".into(), true), ("does_not".into(), false)]);
}

#[test]
fn should_panic_with_a_message() {
    let src = "#[test]\n#[should_panic(\"overflow\")]\nfn right() { panic(\"an overflow happened\") }\n#[test]\n#[should_panic(\"overflow\")]\nfn wrong() { panic(\"something else\") }";
    let r = run(src, None);
    assert_eq!(r.results.iter().map(|t| t.passed).collect::<Vec<_>>(), vec![true, false]);
    assert!(r.results[1].message.contains("does not contain"), "{}", r.results[1].message);
}

#[test]
fn integer_overflow_is_a_test_failure() {
    let src = "#[test]\nfn overflows() {\n let x: u8 = 255\n let y = x + 1\n print(y)\n}";
    let r = run(src, None);
    assert!(!r.results[0].passed);
    assert!(r.results[0].message.contains("integer overflow"));
}

#[test]
fn a_result_returning_test_fails_on_err() {
    let src = "fn check(n: i64) -> Result<i64, str> {\n if n > 0 { Ok(n) } else { Err(\"not positive\") }\n}\n#[test]\nfn passes() -> Result<(), str> {\n let v = check(5)?\n assert_eq(v, 5)\n Ok(())\n}\n#[test]\nfn fails() -> Result<(), str> {\n let v = check(-1)?\n assert_eq(v, 1)\n Ok(())\n}";
    let r = run(src, None);
    assert_eq!(r.results.iter().map(|t| t.passed).collect::<Vec<_>>(), vec![true, false]);
    assert!(r.results[1].message.contains("not positive"), "{}", r.results[1].message);
}

#[test]
fn the_filter_selects_by_name() {
    let src = "#[test]\nfn add_works() {}\n#[test]\nfn add_overflow() {}\n#[test]\nfn mul_works() {}";
    let names = |f: &str| run(src, Some(f)).results.into_iter().map(|t| t.name).collect::<Vec<_>>();
    assert_eq!(names("add"), vec!["add_works", "add_overflow"]);
    assert_eq!(names("works"), vec!["add_works", "mul_works"]);
    assert!(names("nothing").is_empty());
}

#[test]
fn ordinary_functions_and_main_are_not_run() {
    let src = "fn helper() { panic(\"not a test\") }\nfn main() { panic(\"main is not a test\") }\n#[test]\nfn only() { assert(true) }";
    assert_eq!(outcomes(src), vec![("only".into(), true)]);
}

#[test]
fn tests_can_use_the_programs_own_functions_and_types() {
    let src = "struct P { x: i64 }\nimpl P {\n fn double(self) -> i64 { self.x * 2 }\n}\nfn square(n: i64) -> i64 { n * n }\n#[test]\nfn uses_both() {\n assert_eq(P(21).double(), 42)\n assert_eq(square(7), 49)\n}";
    assert_eq!(outcomes(src), vec![("uses_both".into(), true)]);
}

#[test]
fn tests_do_not_leak_state_into_each_other() {
    let src = "#[test]\nfn a() {\n let mut v = Vec<i64>.new()\n v.push(1)\n assert_eq(v.len, 1)\n}\n#[test]\nfn b() {\n let mut v = Vec<i64>.new()\n v.push(1)\n assert_eq(v.len, 1)\n}";
    assert_eq!(outcomes(src), vec![("a".into(), true), ("b".into(), true)]);
}

#[test]
fn a_test_that_never_ends_is_stopped() {
    let r = test_source("#[test]\nfn forever() {\n let mut i = 0\n while true { i += 1 }\n}", None, 100_000).unwrap();
    assert!(!r.results[0].passed);
    assert!(r.results[0].message.contains("step limit"), "{}", r.results[0].message);
}

#[test]
fn malformed_tests_are_rejected_before_anything_runs() {
    for (src, needle) in [
        ("#[test]\nfn takes(x: i64) {}", "cannot take parameters"),
        ("#[test]\nfn generic<T>() {}", "cannot take parameters or be generic"),
        ("#[test]\nfn returns() -> i64 { 1 }", "returns `i64`"),
    ] {
        let r = run(src, None);
        assert!(r.results.is_empty(), "{src}");
        assert!(r.diagnostics.iter().any(|d| d.message.contains(needle)), "{src}: {:?}", r.diagnostics);
    }
}

#[test]
fn type_and_borrow_errors_in_a_test_prevent_the_run() {
    let r = run("#[test]\nfn t() { let x: i64 = \"s\" }", None);
    assert!(r.results.is_empty() && !r.diagnostics.is_empty());
    let r = run("#[test]\nfn t() {\n let a = vec[1]\n let b = a\n print(a.len)\n}", None);
    assert!(r.results.is_empty());
    assert!(r.diagnostics[0].message.contains("moved"));
}

#[test]
fn a_program_without_tests_runs_none() {
    let r = run("fn main() {}", None);
    assert!(r.diagnostics.is_empty() && r.results.is_empty());
}

#[test]
fn the_example_test_file_passes() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/tests/stack.tau")).unwrap();
    let r = run(&src, None);
    assert!(r.diagnostics.is_empty());
    assert_eq!(r.results.len(), 5);
    assert!(r.results.iter().all(|t| t.passed), "{:?}", r.results);
}
