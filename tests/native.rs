//! Differential tests for the native backend: every program here runs through the
//! interpreter (the reference implementation) and through native code, and the two must
//! agree on stdout, on whether the program panicked, and on the panic message.
//!
//! These need a C compiler (clang or gcc); without one they are skipped with a note.

use std::io::Write;
use std::sync::{Arc, Mutex};

use tau::interp::Outcome;
use tau::native::{self, NativeError};

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// What the interpreter does with `src`: (stdout, panic message if any).
fn interpret(src: &str) -> (String, Option<String>) {
    let cap = Capture(Arc::new(Mutex::new(Vec::new())));
    let r = tau::run_source(src, Box::new(cap.clone())).unwrap_or_else(|d| panic!("{}", d.render(src, "<test>")));
    assert!(r.diagnostics.is_empty(), "rejected: {}", r.diagnostics.iter().map(|d| d.render(src, "<test>")).collect::<Vec<_>>().join("\n"));
    let text = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
    match r.outcome.expect("did not run") {
        Outcome::Ok => (text, None),
        Outcome::Panic(m) => (text, Some(m)),
        other => panic!("interpreter failed: {other:?}"),
    }
}

fn have_cc() -> bool {
    native::find_cc().is_some()
}

/// Run `src` both ways and compare. Returns the shared stdout.
fn same(src: &str) -> String {
    if !have_cc() {
        eprintln!("skipped: no C compiler");
        return String::new();
    }
    let (want_out, want_panic) = interpret(src);
    let got = match native::run_captured(src) {
        Ok(g) => g,
        Err(NativeError::Unsupported(why)) => panic!("not supported natively: {why}\n{src}"),
        Err(NativeError::Other(m)) => panic!("{m}\n--- program ---\n{src}"),
        Err(e) => panic!("{e:?}"),
    };
    if got.stdout != want_out {
        // long programs: report the first differing line rather than dumping everything
        let (g, w): (Vec<&str>, Vec<&str>) = (got.stdout.lines().collect(), want_out.lines().collect());
        for (i, (a, b)) in g.iter().zip(&w).enumerate() {
            if a != b {
                let n = src.lines().filter(|l| l.starts_with("print(")).nth(i).unwrap_or("?");
                panic!("stdout differs at output line {i}: native {a:?}, interpreter {b:?} (near {n}); digits native {} interp {}", a.trim_start_matches(|c| c == '0' || c == '.' || c == '-'), b.trim_start_matches(|c| c == '0' || c == '.' || c == '-'));
            }
        }
        assert_eq!(g.len(), w.len(), "stdout has a different number of lines for:\n{src}");
    }
    match want_panic {
        None => {
            assert_eq!(got.code, 0, "native exited with {} ({}) for:\n{src}", got.code, got.stderr);
            assert_eq!(got.stderr, "", "unexpected stderr for:\n{src}");
        }
        Some(m) => {
            assert_eq!(got.code, 101, "native should panic with exit 101 for:\n{src}\nstderr: {}", got.stderr);
            assert_eq!(got.stderr, format!("panic: {m}\n"), "panic message differs for:\n{src}");
        }
    }
    want_out
}

/// Like `same`, for the body of `main`.
fn body(b: &str) -> String {
    same(&format!("fn main() {{\n{b}\n}}"))
}

// ------------------------------------------------------------------ basics

#[test]
fn hello_world() {
    assert_eq!(body("print(\"hello, world\")"), if have_cc() { "hello, world\n" } else { "" });
}

#[test]
fn integer_arithmetic() {
    body("print(7 + 3 * 2)\nprint(7 / 2)\nprint(-7 / 2)\nprint(7 % 3)\nprint(-7 % 3)\nprint(2 ^ 10)");
    body("let a: i8 = -128\nlet b: u16 = 65535\nlet c: i128 = 170141183460469231731687303715884105727\nprint(a)\nprint(b)\nprint(c)\nprint(c / 3)");
    body("let a: u64 = 18446744073709551615\nprint(a)\nprint(a / 7)\nprint(a % 1000)");
    body("let a: usize = 5\nlet b: isize = -5\nprint(a * 3)\nprint(b * 3)");
}

#[test]
fn float_arithmetic_and_formatting() {
    body("print(1.5 + 2.25)\nprint(10.0 / 4.0)\nprint(2.0)\nprint(0.1 + 0.2)\nprint(1.0e10)\nprint(sqrt(2.0))");
    body("print(1.0e21)\nprint(1.0e-7)\nprint(123456789.123456789)\nprint(5e-324)\nprint(1.7976931348623157e308)");
    body("print(1.0 / 0.0)\nprint(-1.0 / 0.0)\nprint(0.0 / 0.0)\nprint(-0.0)");
    body("let x: f32 = 0.1\nprint(x)\nprint(x + 0.2)\nprint(x * 3.0)\nlet y: f32 = 16777216.0\nprint(y + 1.0)");
}

#[test]
fn mixed_literals_become_floats() {
    body("let x = 2.5\nprint(2 * x)\nlet y: f64 = 3\nprint(y)");
}

#[test]
fn precedence_and_power() {
    body("print(2 ^ 3 ^ 2)\nprint(-2 ^ 2)\nprint(1 + 2 * 3 ^ 2)\nprint((1 + 2) * 3)\nprint(2.0 ^ 0.5)\nprint(2.0 ^ 10.0)");
}

#[test]
fn comparison_and_logic() {
    body("print(1 < 2)\nprint(2 <= 1)\nprint(\"a\" < \"b\")\nprint(1 == 1 && 2 != 3)\nprint(!(1 > 2) || false)\nprint(\"abc\" == \"abc\")\nprint(\"b\" > \"abc\")");
    body("print(1.5 < 2.5)\nprint((1, 2) < (1, 3))\nprint((1, \"a\") == (1, \"a\"))\nprint('a' < 'b')");
    body("let n = 0.0 / 0.0\nprint(n == n)\nprint(n != n)\nprint(n < 1.0)\nprint(n >= 1.0)");
}

#[test]
fn short_circuit_evaluation() {
    same("fn t(s: str) -> bool {\n print(s)\n true\n}\nfn f(s: str) -> bool {\n print(s)\n false\n}\nfn main() {\n print(f(\"a\") && t(\"b\"))\n print(t(\"c\") || f(\"d\"))\n print(t(\"e\") && t(\"f\"))\n}");
}

#[test]
fn bitwise_and_shifts() {
    body("print(6 & 3)\nprint(6 | 3)\nprint(6 ^^ 3)\nprint(1 << 10)\nprint(1024 >> 3)\nprint(~0)");
    body("let a: u8 = 255\nprint(~a)\nprint(a >> 4)\nlet b: i32 = -16\nprint(b >> 2)");
    body("let n = 70\nprint(1 << n)");
    body("let n: i32 = 32\nprint(1i32 << n)");
}

#[test]
fn casts() {
    body("print(300 as u8)\nprint(-1 as u8)\nprint(3.99 as i32)\nprint(-3.99 as i32)\nprint(300.0 as u8)\nprint(65u8 as char)\nprint('a' as u32)\nprint(7 as f64 / 2.0)\nprint(true as i32)");
    body("print(1.0e30 as i64)\nprint(-1.0e30 as i64)\nprint((0.0 / 0.0) as i32)\nprint(3000000000 as i32)\nprint(-1 as u64)\nprint(255 as i8)");
    body("let x: f32 = 1.1\nprint(x as f64)\nprint(1.1 as f32)\nprint(16777217 as f32)");
}

#[test]
fn strings_and_interpolation() {
    body("let n = 3\nlet s = \"x\"\nprint(\"n = {n}, s = {s}, sum = {n + 4}\")\nprint(\"brace: \\{ok\\}\")");
    body("let t = (1, 2.5, \"q\")\nprint(\"t = {t}\")\nprint(\"{1}{2}{3}\")\nlet c = 'z'\nprint(\"c={c} b={true} u={()}\")");
    body("print(\"tab\\there\\nnewline \\\"quoted\\\" back\\\\slash\")\nprint(\"unicode: é ✓ 日本\")\nprint('é')");
    body("let s = \"a{1}b\"\nlet u = \"<{s}>\"\nprint(u)");
}

#[test]
fn tuples() {
    body("print((1, \"a\", 2.5))\nprint(())\nprint((1,))\nlet t = (1, (2, 3))\nprint(t)\nprint(t.1.0)\nlet (a, (b, c)) = t\nprint(a + b + c)");
    body("let mut t = (1, 2)\nt.0 = 10\nt.1 += 5\nprint(t)");
}

// ------------------------------------------------------------------ overflow and panics

#[test]
fn integer_overflow_panics_at_the_static_width() {
    body("let x: i32 = 2147483647\nlet y = x + 1\nprint(y)");
    body("let x: u8 = 0\nlet y = x - 1\nprint(y)");
    body("let x: i64 = 9223372036854775807\nprint(x * 2)");
    body("let x: u8 = 200\nlet y = x as u32 + 100\nprint(y)");
    body("let x: i8 = -128\nprint(-x)");
    body("let x: i64 = -9223372036854775807 - 1\nprint(x / -1)");
    body("let x: i128 = 170141183460469231731687303715884105727\nprint(x + 1)");
    body("let x: u32 = 4294967295\nprint(x * x)");
    body("print(2 ^ 63)");
    body("print(3 ^ 40)");
    body("let e = -1\nprint(2 ^ e)");
    body("let x: u64 = 18446744073709551615\nprint(x + 1)");
}

#[test]
fn division_by_zero_panics() {
    body("let z = 0\nprint(1 / z)");
    body("let z = 0\nprint(1 % z)");
    body("print(1.0 / 0.0)");
}

#[test]
fn explicit_panics_and_assertions() {
    body("panic(\"boom\")");
    body("assert(1 > 2)");
    body("assert_eq(1 + 1, 3)");
    body("assert_eq(\"a\", \"b\")");
    body("assert_eq((1, 2.5), (1, 2.0))");
    body("assert(1 < 2)\nassert_eq(2 + 2, 4)\nprint(\"fine\")");
    body("let n = 7\npanic(\"bad value {n}\")");
}

#[test]
fn output_before_a_panic_is_kept() {
    let out = same("fn main() {\n print(\"before\")\n panic(\"stop\")\n print(\"after\")\n}");
    if have_cc() {
        assert_eq!(out, "before\n");
    }
}

#[test]
fn lots_of_output_is_flushed_in_order() {
    body("for i in 0..50000 { print(i) }\npanic(\"done\")");
}

#[test]
fn runaway_recursion_is_a_stack_overflow_panic() {
    if !have_cc() {
        return;
    }
    let src = "fn f(n: i64) -> i64 { f(n + 1) + 1 }\nfn main() { print(f(0)) }";
    let got = native::run_captured(src).unwrap();
    assert_eq!(got.code, 101, "{got:?}");
    assert!(got.stderr.starts_with("panic: stack overflow"), "{got:?}");
}

#[test]
fn deep_but_finite_recursion_works() {
    same("fn sum(n: i64) -> i64 { if n == 0 { 0 } else { n + sum(n - 1) } }\nfn main() { print(sum(2000))\n print(sum(1500)) }");
}

// ------------------------------------------------------------------ control flow

#[test]
fn loops() {
    body("let mut t = 0\nfor i in 1..=10 { t += i }\nprint(t)");
    body("let mut n = 0\nwhile n < 5 { n += 2 }\nprint(n)");
    body("let mut i = 0\nlet r = loop {\n i += 3\n if i > 10 { break i * 2 }\n}\nprint(r)");
    body("for i in 5..5 { print(i) }\nfor i in 5..=4 { print(i) }\nfor i in 3..=3 { print(i) }");
    body("for i in 250u8..=255 { print(i) }");
    body("for i in -3..3 { print(i) }");
}

#[test]
fn break_and_continue() {
    body("let mut s = 0\nfor i in 0..10 {\n if i % 2 == 0 { continue }\n if i > 7 { break }\n s += i\n}\nprint(s)");
    body("let mut s = 0\nfor i in 0..5 {\n for j in 0..5 {\n if j > i { break }\n if j == 1 { continue }\n s += i * 10 + j\n }\n}\nprint(s)");
    body("let mut n = 0\nwhile true {\n n += 1\n if n == 4 { continue }\n if n > 6 { break }\n print(n)\n}");
    body("let mut i = 0\nlet r = loop {\n i += 1\n let mut j = 0\n let q = loop {\n j += 1\n if j == i { break j * 100 }\n }\n if q > 300 { break q + i }\n}\nprint(r)");
}

#[test]
fn for_with_a_where_guard() {
    body("let mut s = 0\nfor i in 0..10 where i % 3 == 0 { s += i }\nprint(s)");
}

#[test]
fn if_is_an_expression() {
    body("let x = if 3 > 2 { \"yes\" } else { \"no\" }\nprint(x)\nlet y = if false { 1 } else if true { 2 } else { 3 }\nprint(y)");
    body("let n = 5\nif n > 3 { print(\"big\") }\nif n > 9 { print(\"huge\") } else { print(\"not huge\") }");
}

#[test]
fn match_on_values_ranges_and_guards() {
    same("fn grade(n: i64) -> str {\n match n {\n 90..=100 => \"A\"\n 80..=89 => \"B\"\n x if x < 0 => \"invalid\"\n 0 | 1 | 2 => \"tiny\"\n _ => \"other\"\n }\n}\nfn main() {\n print(grade(95))\n print(grade(85))\n print(grade(-4))\n print(grade(1))\n print(grade(50))\n}");
    body("let c = 'q'\nlet k = match c {\n 'a'..='m' => 1\n 'n'..='z' => 2\n _ => 3\n}\nprint(k)");
    body("let s = \"two\"\nprint(match s {\n \"one\" => 1\n \"two\" => 2\n _ => 0\n})");
    body("print(match true {\n true => \"t\"\n false => \"f\"\n})");
}

#[test]
fn match_on_tuples() {
    same("fn main() {\n for i in 1..=15 {\n match (i % 3, i % 5) {\n (0, 0) => print(\"FizzBuzz\")\n (0, _) => print(\"Fizz\")\n (_, 0) => print(\"Buzz\")\n _ => print(i)\n }\n }\n}");
    body("let (a, b) = (3, 4)\nlet r = match (a, b) {\n (x, y) if x > y => x\n (x, y) => y * 10 + x\n}\nprint(r)");
}

#[test]
fn early_return() {
    same("fn first_big(n: i64) -> i64 {\n for i in 0..n {\n if i * i > 50 { return i }\n }\n -1\n}\nfn main() { print(first_big(100))\n print(first_big(3)) }");
    same("fn f(x: i64) {\n if x > 2 { return }\n print(x)\n}\nfn main() { f(1)\n f(5)\n f(2) }");
}

// ------------------------------------------------------------------ functions and values

#[test]
fn functions_and_recursion() {
    same("fn fib(n: i64) -> i64 { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }\nfn main() { print(fib(20)) }");
    same("fn is_even(n: i64) -> bool { if n == 0 { true } else { is_odd(n - 1) } }\nfn is_odd(n: i64) -> bool { if n == 0 { false } else { is_even(n - 1) } }\nfn main() { print(is_even(10))\n print(is_odd(10)) }");
    same("fn gcd(a: u64, b: u64) -> u64 { if b == 0 { a } else { gcd(b, a % b) } }\nfn main() { print(gcd(48, 18))\n print(gcd(1071, 462)) }");
    same("fn swap(p: (i64, str)) -> (str, i64) { (p.1, p.0) }\nfn main() { print(swap((1, \"a\"))) }");
}

#[test]
fn evaluation_order_is_left_to_right() {
    same("fn n(s: str, v: i64) -> i64 {\n print(s)\n v\n}\nfn add3(a: i64, b: i64, c: i64) -> i64 { a * 100 + b * 10 + c }\nfn main() {\n print(n(\"a\", 1) - n(\"b\", 2))\n print(add3(n(\"x\", 1), n(\"y\", 2), n(\"z\", 3)))\n print((n(\"p\", 1), n(\"q\", 2)))\n print(n(\"l\", 1) < n(\"r\", 2))\n}");
}

#[test]
fn mut_scalar_parameters() {
    same("fn bump(x: mut i64) { x += 1 }\nfn twice(x: mut i64) {\n bump(mut x)\n bump(mut x)\n}\nfn main() {\n let mut a = 5\n bump(mut a)\n twice(mut a)\n print(a)\n}");
    same("fn fill(t: mut (i64, i64)) { t.0 = 7\n t.1 = 8 }\nfn main() {\n let mut p = (0, 0)\n fill(mut p)\n print(p)\n}");
}

#[test]
fn shadowing_and_blocks() {
    body("let x = 1\nlet x = x + 1\n{ let x = x * 10\n print(x) }\nprint(x)\nlet y = { let a = 3\n a * a }\nprint(y)");
    body("let x = 1\nlet x = \"now a string\"\nprint(x)");
}

#[test]
fn constants() {
    same("const LIMIT: i64 = 10\nconst SCALE: f64 = 2.5\nconst TWICE: i64 = LIMIT * 2\nfn main() { print(LIMIT)\n print(SCALE * 2.0)\n print(TWICE) }");
    body("print(PI)\nprint(TAU)\nprint(E)");
}

#[test]
fn math_builtins() {
    body("print(sqrt(16.0))\nprint(sin(0.0))\nprint(cos(0.0))\nprint(exp(1.0))\nprint(ln(2.0))\nprint(floor(2.7))\nprint(ceil(2.1))\nprint(round(2.5))\nprint(atan2(1.0, 1.0))\nprint(abs(-3.5))\nprint(abs(-4))\nprint(min(3, 9))\nprint(max(3, 9))\nprint(min(2.5, 1.5))\nprint(max(\"a\", \"b\"))");
    body("let x: f32 = 2.0\nprint(sqrt(x))\nprint(sin(x))\nprint(exp(x))");
    body("print(abs(-128i8))");
    body("print(log10(1000.0))\nprint(tan(0.5))\nprint(asin(0.5))\nprint(acos(0.5))\nprint(atan(1.0))");
}

// ------------------------------------------------------------------ whole programs

#[test]
fn collatz_example() {
    let src = std::fs::read_to_string("examples/run/collatz.tau").unwrap();
    let want = std::fs::read_to_string("examples/run/collatz.expected").unwrap();
    if have_cc() {
        assert_eq!(same(&src), want);
    }
}

#[test]
fn fizzbuzz_example() {
    let src = std::fs::read_to_string("examples/fizzbuzz.tau").unwrap();
    same(&src);
}

#[test]
fn unsupported_programs_say_so_instead_of_miscompiling() {
    for src in [
        "fn main() { let v = vec[1, 2, 3]\n print(v[0]) }",
        "fn main() { let f = |x: i64| x + 1\n print(f(1)) }",
        "fn id<T: Copy>(x: T) -> T { x }\nfn main() { print(id(1)) }",
        "fn main() { let t = Tensor<f64, [2]>.ones()\n print(t) }",
    ] {
        match native::emit_c(src) {
            Err(NativeError::Unsupported(_)) => {}
            Ok(_) => panic!("should be unsupported: {src}"),
            Err(e) => panic!("{e:?} for {src}"),
        }
    }
}

#[test]
fn programs_with_errors_are_rejected_before_codegen() {
    assert!(matches!(native::emit_c("fn main() { let x: i64 = \"a\" }"), Err(NativeError::Rejected(_))));
    assert!(matches!(native::emit_c("fn main( {"), Err(NativeError::Parse(_))));
    assert!(matches!(native::emit_c("fn helper() {}"), Err(NativeError::Other(_))));
}

// ------------------------------------------------------------------ float formatting

/// Tau source text for a float literal that reads back as exactly `x`.
fn lit(x: f64) -> String {
    let s = format!("{x:e}");
    match s.split_once('e') {
        Some((m, e)) if !m.contains('.') => format!("{m}.0e{e}"),
        _ => s,
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Shortest round-trip printing is the hardest part of matching the interpreter, so
/// hammer it: random bit patterns, short decimals, powers of two and ten, and f32 values.
#[test]
fn float_printing_matches_the_interpreter_on_many_values() {
    if !have_cc() {
        return;
    }
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut vals: Vec<f64> = Vec::new();
    while vals.len() < 2500 {
        let x = f64::from_bits(rng.next());
        if x.is_finite() {
            vals.push(x);
        }
    }
    for _ in 0..1500 {
        let m = (rng.next() % 2_000_000) as f64 - 1_000_000.0;
        let k = (rng.next() % 12) as i32;
        vals.push(m / 10f64.powi(k));
        vals.push(m * 10f64.powi(k));
    }
    for e in -1074..=1023 {
        vals.push(2f64.powi(e.max(-1022)));
        vals.push(-(2f64.powi(e.clamp(-1022, 1023)) * 1.5));
    }
    for e in -300..=300 {
        vals.push(10f64.powi(e));
    }
    vals.extend([0.0, 1.0, -1.0, 0.1, 0.5, 1e15, 1e16, 1e17, 123456789012345680.0, 4.35, 0.3, 2.5e-5, f64::MAX, f64::MIN_POSITIVE, 5e-324]);
    // many small functions: one huge `main` makes the C compiler very slow
    let mut lines: Vec<String> = vals.iter().map(|x| format!("print({})\n", lit(*x))).collect();
    for _ in 0..1500 {
        let x = f32::from_bits(rng.next() as u32);
        if x.is_finite() {
            lines.push(format!("let f: f32 = {}\nprint(f)\n", lit(x as f64)));
        }
    }
    for k in 1..60 {
        lines.push(format!("let g: f32 = {}\nprint(g)\nprint(g * g)\n", lit(1.0 / (k as f64))));
    }
    let mut prog = String::new();
    let mut calls = String::new();
    for (i, chunk) in lines.chunks(50).enumerate() {
        prog.push_str(&format!("fn part{i}() {{\n{}}}\n", chunk.concat()));
        calls.push_str(&format!("part{i}()\n"));
    }
    prog.push_str(&format!("fn main() {{\n{calls}}}\n"));
    same(&prog);
}
