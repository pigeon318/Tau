//! The checker must never panic on any program the parser accepts.

use std::fs;
use std::path::PathBuf;

fn sources() -> Vec<(String, String)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut out = Vec::new();
    for dir in [root.clone(), root.join("checked")] {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) == Some("tau") {
                out.push((
                    path.file_name().unwrap().to_string_lossy().to_string(),
                    fs::read_to_string(&path).unwrap(),
                ));
            }
        }
    }
    out
}

/// Parse `src`; if it parses, check it. Neither may panic.
fn parse_and_check(src: &str) {
    thread_local! {
        static LIB: Vec<tau::prelude::ModuleSrc> = tau::prelude::load();
    }
    if let Ok(user) = tau::parse_module(src) {
        LIB.with(|lib| {
            let _ = tau::check(lib, &user);
        });
    }
}

#[test]
fn truncated_programs_never_panic() {
    for (name, src) in sources() {
        for (i, _) in src.char_indices() {
            let cut = &src[..i];
            let r = std::panic::catch_unwind(|| parse_and_check(cut));
            assert!(r.is_ok(), "panic checking {name} truncated at byte {i}");
        }
    }
}

#[test]
fn programs_with_one_char_deleted_never_panic() {
    for (name, src) in sources() {
        for (i, _) in src.char_indices().step_by(2) {
            let mut s = src.clone();
            s.remove(i);
            let r = std::panic::catch_unwind(|| parse_and_check(&s));
            assert!(r.is_ok(), "panic checking {name} with byte {i} deleted");
        }
    }
}

#[test]
fn programs_with_a_word_swapped_never_panic() {
    // swap identifiers and types around so the program still parses but means nothing
    let swaps = [("f64", "i64"), ("i64", "bool"), ("Vec3", "Body"), ("self", "x"), ("mut ", ""), ("Some", "Ok"), ("usize", "str"), ("->", "-> Option<i64> //"), ("(", "(1, ")];
    for (name, src) in sources() {
        for (from, to) in swaps {
            let mut start = 0;
            while let Some(pos) = src[start..].find(from) {
                let at = start + pos;
                let mut s = src.clone();
                s.replace_range(at..at + from.len(), to);
                let r = std::panic::catch_unwind(|| parse_and_check(&s));
                assert!(r.is_ok(), "panic checking {name} replacing {from:?} at {at}");
                start = at + from.len();
            }
        }
    }
}

#[test]
fn diagnostics_render_without_panicking() {
    // every diagnostic span must be renderable against its source
    for (name, src) in sources() {
        for (i, _) in src.char_indices().step_by(5) {
            let mut s = src.clone();
            s.remove(i);
            if let Ok(r) = tau::check_source(&s) {
                for d in &r.diagnostics {
                    let _ = d.render(&s, &name);
                }
            }
        }
    }
}

#[test]
fn junk_insertion_never_panics() {
    let junk = [
        "(", ")", "{", "}", "[", "]", ",", ".", "mut ", "own ", "par ", "spawn ", "on ", "unsafe ",
        "?", "..", "@", "^", "<", ">", "=", "|x| ", "Self", "self", "match ", "_", "0", "\"x\"",
        "Vec", "Tensor<f64, [_]>", "fn", "impl", "->",
    ];
    for (name, src) in sources() {
        for (i, _) in src.char_indices().step_by(7) {
            for j in junk {
                let mut s = src.clone();
                s.insert_str(i, j);
                let r = std::panic::catch_unwind(|| parse_and_check(&s));
                assert!(r.is_ok(), "panic checking {name} inserting {j:?} at byte {i}");
            }
        }
    }
}

#[test]
fn adversarial_snippets_never_panic() {
    let snippets = [
        "fn f() { f() }",
        "fn f() -> Never { f() }",
        "struct S { s: S }",
        "struct S<T> { t: S<T> }",
        "enum E { A(E) }",
        "type A = A",
        "trait T { fn f(self) -> Self }\nimpl T for T {}",
        "impl Add for i64 { fn add(self, o: i64) -> i64 { self } }",
        "impl Vec<i64> {}\nimpl<T> T {}",
        "fn f<T>() -> T { f() }\nfn main() { let x = f() }",
        "fn main() { let x = x }",
        "fn main() { let f = |x| f(x) }",
        "fn main() { let f = |x| x(x) }",
        "fn main() { let a = [a] }",
        "fn main() { 1.foo.bar.baz }",
        "fn main() { Vec.Vec.Vec() }",
        "fn main() { Option.None.Some() }",
        "fn main() { Some.new() }",
        "fn main() { match () { () => () } }",
        "fn main() { match 1 { 1..=2 => 0, _ => 1 } }",
        "fn main() { for (a, (b, c)) in 0..3 {} }",
        "fn main() { let (a, b, c) = (1, 2) }",
        "fn main() { let [a] = [1] }",
        "fn main() { spawn { return 1 } }",
        "fn main() { on local { break } }",
        "fn main() { loop { loop { break 1 } break } }",
        "fn main() { let x: _ = 1 }",
        "fn main() { let x: Tensor<f64, [_]> = 1 }",
        "fn f(self) {}\nfn g(a: self) {}",
        "impl X { fn f(self) {} }",
        "trait T { const C: i64 = 1 }",
        "fn main() { (|| 1)() }",
        "fn main() { let t = (1,); t.5 }",
        "fn main() { ..; ..=1; 1.. }",
        "use std.map.*\nuse std.fs.*\nfn main() { Map<i64, i64>.new() }",
        "use a.b.c.d.e\nuse std.fs.nope.more",
        "fn main() { print(print) }",
        "fn main() { vec[vec[vec[]]] }",
        "fn main() { let v = vec[]\n v.push(1)\n v.push(\"s\") }",
        "cluster c = nodes [1, 2]\nfn main() { on c { 1 } }",
        "cluster c = nodes []\nfn main() { on c { } on 5 { } }",
        "fn main() { 1 as Vec }",
        "fn main() { let x = 1\n x as }",
    ];
    for src in snippets {
        let r = std::panic::catch_unwind(|| parse_and_check(src));
        assert!(r.is_ok(), "panic checking {src:?}");
    }
}

// ---- borrow checker: mutate whole lines so programs still type-check

fn analyze_src(src: &str) -> Option<usize> {
    thread_local! {
        static LIB: Vec<tau::prelude::ModuleSrc> = tau::prelude::load();
    }
    let user = tau::parse_module(src).ok()?;
    LIB.with(|lib| Some(tau::analyze(lib, &user).diagnostics.len()))
}

fn checked_sources() -> Vec<(String, String)> {
    sources().into_iter().filter(|(n, _)| !n.starts_with("features") && !n.starts_with("cluster")).collect()
}

#[test]
fn borrow_checker_survives_line_deletion_duplication_and_swaps() {
    let mut analysed = 0;
    for (name, src) in checked_sources() {
        let lines: Vec<&str> = src.lines().collect();
        let mut variants: Vec<String> = Vec::new();
        for i in 0..lines.len() {
            // delete line i
            let mut v = lines.clone();
            v.remove(i);
            variants.push(v.join("\n"));
            // duplicate line i
            let mut v = lines.clone();
            v.insert(i, lines[i]);
            variants.push(v.join("\n"));
            // swap lines i and i+1
            if i + 1 < lines.len() {
                let mut v = lines.clone();
                v.swap(i, i + 1);
                variants.push(v.join("\n"));
            }
        }
        for v in variants {
            let r = std::panic::catch_unwind(|| analyze_src(&v));
            match r {
                Ok(Some(_)) => analysed += 1,
                Ok(None) => {}
                Err(_) => panic!("borrow checker panicked on a mutation of {name}:\n{v}"),
            }
        }
    }
    assert!(analysed > 200, "only {analysed} mutations parsed; the test is not exercising anything");
}

#[test]
fn analysis_is_deterministic() {
    for (name, src) in checked_sources() {
        let a = analyze_src(&src);
        let b = analyze_src(&src);
        assert_eq!(a, b, "{name}");
    }
}
