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

// ------------------------------------------------------------------ structs, enums and generics

#[test]
fn structs() {
    same("struct P { x: i64, y: f64 }\nfn main() {\n let p = P(3, 2.5)\n print(p.x)\n print(p.y * 2.0)\n let mut q = P(1, 1.0)\n q.x = 10\n q.y += 0.5\n print(q.x)\n print(q.y)\n}");
    same("struct Pair { a: (i64, i64), b: bool }\nfn main() {\n let p = Pair((1, 2), true)\n print(p.a.1)\n let Pair((x, y), flag) = p\n print(x + y)\n print(flag)\n}");
    same("struct Unit {}\nstruct One { v: i8 }\nfn main() {\n let u = Unit()\n let o = One(5)\n print(o.v)\n}");
    same("struct Acc { total: i64 }\nfn bump(a: mut Acc, by: i64) { a.total += by }\nfn read(a: Acc) -> i64 { a.total }\nfn main() {\n let mut a = Acc(0)\n bump(mut a, 5)\n bump(mut a, 6)\n print(read(a))\n}");
    same("struct In { v: i64 }\nstruct Out { i: In, w: i64 }\nfn main() {\n let mut o = Out(In(1), 2)\n o.i.v = 7\n o.i.v += 1\n print(o.i.v + o.w)\n}");
}

#[test]
fn enums_and_match() {
    same("enum Shape { Circle(f64), Rect(f64, f64), Empty }\nfn area(s: Shape) -> f64 {\n match s {\n Circle(r) => 3.0 * r * r\n Rect(w, h) => w * h\n Empty => 0.0\n }\n}\nfn main() {\n print(area(Circle(2.0)))\n print(area(Rect(2.0, 3.5)))\n print(area(Empty))\n}");
    same("enum Color { Red, Green, Blue }\nfn name(c: Color) -> str {\n match c {\n Red => \"red\"\n Green => \"green\"\n Blue => \"blue\"\n }\n}\nfn main() {\n print(name(Green))\n print(name(Color.Blue))\n let c = Color.Red\n print(name(c))\n}");
    same("enum T { A(i64), B(i64, i64) }\nfn f(t: T) -> i64 {\n match t {\n A(x) if x > 5 => x\n A(x) => 0 - x\n B(x, y) => x * y\n }\n}\nfn main() {\n print(f(A(9)))\n print(f(A(2)))\n print(f(B(3, 4)))\n}");
    same("enum E { V(i64) }\nfn main() {\n let e = V(4)\n let r = match e { V(x) => x + 1 }\n print(r)\n}");
}

#[test]
fn option_and_result() {
    same("fn find(n: i64) -> Option<i64> { if n > 0 { Some(n * 2) } else { None } }\nfn main() {\n match find(4) {\n Some(v) => print(\"got {v}\")\n None => print(\"none\")\n }\n match find(-1) {\n Some(v) => print(\"got {v}\")\n None => print(\"none\")\n }\n}");
    same("fn div(a: i64, b: i64) -> Result<i64, str> { if b == 0 { Err(\"div by zero\") } else { Ok(a / b) } }\nfn main() {\n for d in 0..3 {\n match div(6, d) {\n Ok(v) => print(v)\n Err(m) => print(m)\n }\n }\n}");
    same("fn half(n: i64) -> Result<i64, str> { if n % 2 == 0 { Ok(n / 2) } else { Err(\"odd\") } }\nfn quarter(n: i64) -> Result<i64, str> {\n let h = half(n)?\n let q = half(h)?\n Ok(q)\n}\nfn main() {\n print(match quarter(8) { Ok(v) => v, Err(_) => -1 })\n print(match quarter(6) { Ok(v) => v, Err(_) => -1 })\n print(match quarter(7) { Ok(v) => v, Err(_) => -2 })\n}");
    same("fn pos(n: i64) -> Option<i64> { if n > 0 { Some(n) } else { None } }\nfn sum2(a: i64, b: i64) -> Option<i64> {\n let x = pos(a)?\n let y = pos(b)?\n Some(x + y)\n}\nfn main() {\n print(match sum2(1, 2) { Some(v) => v, None => 0 })\n print(match sum2(1, -2) { Some(v) => v, None => 0 })\n}");
    same("fn main() {\n let a: Option<f64> = Some(1.5)\n let b: Result<(), str> = Ok(())\n let c: Option<(i64, bool)> = Some((1, true))\n match c {\n Some((n, f)) => print(\"{n} {f}\")\n None => print(\"none\")\n }\n match b {\n Ok(_) => print(\"ok\")\n Err(e) => print(e)\n }\n match a {\n Some(x) => print(x)\n None => print(0.0)\n }\n}");
}

#[test]
fn methods_and_associated_functions() {
    same("struct C { n: i64 }\nimpl C {\n fn new() -> C { C(0) }\n fn bump(mut self) { self.n += 1 }\n fn get(self) -> i64 { self.n }\n fn plus(self, k: i64) -> C { C(self.n + k) }\n}\nfn main() {\n let mut c = C.new()\n c.bump()\n c.bump()\n print(c.get())\n print(c.plus(5).get())\n}");
    same("struct V { x: f64, y: f64 }\nimpl V {\n fn dot(self, o: V) -> f64 { self.x * o.x + self.y * o.y }\n fn norm(self) -> f64 { sqrt(self.dot(self)) }\n}\nfn main() {\n let v = V(3.0, 4.0)\n print(v.norm())\n print(v.dot(V(1.0, 2.0)))\n}");
}

#[test]
fn operator_overloading() {
    same("struct V { x: f64, y: f64 }\nimpl Copy for V {}\nimpl Add for V { fn add(self, o: V) -> V { V(self.x + o.x, self.y + o.y) } }\nimpl Sub for V { fn sub(self, o: V) -> V { V(self.x - o.x, self.y - o.y) } }\nimpl Mul for V { fn mul(self, k: f64) -> V { V(self.x * k, self.y * k) } }\nimpl Neg for V { fn neg(self) -> V { V(0.0 - self.x, 0.0 - self.y) } }\nfn main() {\n let a = V(1.0, 2.0)\n let b = V(0.5, 0.25)\n let c = (a + b) * 2.0 - a\n print(c.x)\n print(c.y)\n let mut d = a\n d += b\n d *= 3.0\n print(d.x)\n let n = -a\n print(n.y)\n}");
    same("struct M { v: i64 }\nimpl Copy for M {}\nimpl Eq for M { fn eq(self, o: M) -> bool { self.v == o.v } }\nimpl Ord for M { fn lt(self, o: M) -> bool { self.v < o.v } }\nfn main() {\n let a = M(1)\n let b = M(2)\n print(a == b)\n print(a != b)\n print(a < b)\n print(a > b)\n print(a <= b)\n print(b >= a)\n print(a == M(1))\n}");
}

#[test]
fn generic_functions_and_types() {
    same("fn id<T: Copy>(x: T) -> T { x }\nfn main() {\n print(id(5))\n print(id(2.5))\n print(id(\"s\"))\n print(id((1, 2)))\n}");
    same("struct Box2<T> { a: T, b: T }\nfn swap2<T: Copy>(p: Box2<T>) -> Box2<T> { Box2(p.b, p.a) }\nfn main() {\n let p = swap2(Box2(1, 2))\n print(p.a)\n let q = swap2(Box2(1.5, 2.5))\n print(q.a)\n}");
    same("enum Maybe<T> { Just(T), Nothing }\nfn unwrap_or<T: Copy>(m: Maybe<T>, d: T) -> T {\n match m {\n Just(v) => v\n Nothing => d\n }\n}\nfn main() {\n print(unwrap_or(Just(3), 0))\n print(unwrap_or(Maybe.Nothing, 9))\n print(unwrap_or(Just(1.5), 0.0))\n}");
    same("fn pick<T: Ord + Copy>(a: T, b: T) -> T { if a < b { b } else { a } }\nfn main() {\n print(pick(1, 2))\n print(pick(2.5, 1.5))\n print(pick(\"a\", \"b\"))\n}");
}

#[test]
fn traits_with_static_dispatch() {
    same("trait Shape {\n fn area(self) -> f64\n fn name(self) -> str { \"shape\" }\n}\nstruct Sq { s: f64 }\nstruct Ci { r: f64 }\nimpl Shape for Sq {\n fn area(self) -> f64 { self.s * self.s }\n fn name(self) -> str { \"square\" }\n}\nimpl Shape for Ci { fn area(self) -> f64 { 3.0 * self.r * self.r } }\nfn describe<T: Shape>(x: T) {\n print(x.name())\n print(x.area())\n}\nfn main() {\n describe(Sq(2.0))\n describe(Ci(1.0))\n}");
    same("trait Zero { fn zero() -> Self }\nimpl Zero for i64 { fn zero() -> i64 { 0 } }\nimpl Zero for f64 { fn zero() -> f64 { 0.0 } }\nfn z<T: Zero>() -> T { T.zero() }\nfn main() {\n let a: i64 = z()\n let b: f64 = z()\n print(a)\n print(b)\n}");
}
