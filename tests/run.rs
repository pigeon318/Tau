//! Running programs: every expected output below was worked out by hand.

use std::io::Write;
use std::sync::{Arc, Mutex};

use tau::interp::Outcome;

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

/// Run `src`; return what it printed and how it ended. Panics if it was rejected.
fn run(src: &str) -> (String, Outcome) {
    let cap = Capture(Arc::new(Mutex::new(Vec::new())));
    let r = tau::run_source(src, Box::new(cap.clone())).unwrap_or_else(|d| panic!("{}", d.render(src, "<test>")));
    assert!(
        r.diagnostics.is_empty(),
        "the program was rejected:\n{}",
        r.diagnostics.iter().map(|d| d.render(src, "<test>")).collect::<Vec<_>>().join("\n")
    );
    let text = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
    (text, r.outcome.expect("the program did not run"))
}

/// Run `main`'s body and return its output, which must end normally.
fn out(body: &str) -> String {
    let (text, outcome) = run(&format!("fn main() {{\n{body}\n}}"));
    assert_eq!(outcome, Outcome::Ok, "output so far: {text}");
    text
}

/// Run a whole program; return its output.
fn out_prog(src: &str) -> String {
    let (text, outcome) = run(src);
    assert_eq!(outcome, Outcome::Ok, "output so far: {text}");
    text
}

/// The program must panic with a message containing `needle`.
fn panics(body: &str, needle: &str) {
    let (_, outcome) = run(&format!("fn main() {{\n{body}\n}}"));
    match outcome {
        Outcome::Panic(m) => assert!(m.contains(needle), "expected a panic containing {needle:?}, got {m:?}"),
        other => panic!("expected a panic containing {needle:?}, got {other:?}"),
    }
}

// ------------------------------------------------------------------ basics

#[test]
fn hello_world() {
    assert_eq!(out("print(\"hello, world\")"), "hello, world\n");
}

#[test]
fn integer_arithmetic() {
    assert_eq!(out("print(7 + 3 * 2)\nprint(7 / 2)\nprint(-7 / 2)\nprint(7 % 3)\nprint(-7 % 3)\nprint(2 ^ 10)"), "13\n3\n-3\n1\n-1\n1024\n");
}

#[test]
fn float_arithmetic_and_formatting() {
    assert_eq!(out("print(1.5 + 2.25)\nprint(10.0 / 4.0)\nprint(2.0)\nprint(0.1 + 0.2)\nprint(1.0e10)\nprint(sqrt(2.0))"), "3.75\n2.5\n2.0\n0.30000000000000004\n10000000000.0\n1.4142135623730951\n");
}

#[test]
fn mixed_literals_become_floats() {
    assert_eq!(out("let x = 2.5\nprint(2 * x)\nlet y: f64 = 3\nprint(y)"), "5.0\n3.0\n");
}

#[test]
fn precedence_and_power() {
    assert_eq!(out("print(2 ^ 3 ^ 2)\nprint(-2 ^ 2)\nprint(1 + 2 * 3 ^ 2)\nprint((1 + 2) * 3)"), "512\n-4\n19\n9\n");
}

#[test]
fn comparison_and_logic() {
    assert_eq!(out("print(1 < 2)\nprint(2 <= 1)\nprint(\"a\" < \"b\")\nprint(1 == 1 && 2 != 3)\nprint(!(1 > 2) || false)"), "true\nfalse\ntrue\ntrue\ntrue\n");
}

#[test]
fn bitwise_and_shifts() {
    assert_eq!(out("print(6 & 3)\nprint(6 | 3)\nprint(6 ^^ 3)\nprint(1 << 10)\nprint(1024 >> 3)\nprint(~0)"), "2\n7\n5\n1024\n128\n-1\n");
}

#[test]
fn casts() {
    assert_eq!(
        out("print(300 as u8)\nprint(-1 as u8)\nprint(3.99 as i32)\nprint(-3.99 as i32)\nprint(300.0 as u8)\nprint(65u8 as char)\nprint('a' as u32)\nprint(7 as f64 / 2.0)\nprint(true as i32)"),
        "44\n255\n3\n-3\n255\n65\n97\n3.5\n1\n".replace("65\n97", "A\n97")
    );
}

#[test]
fn strings_and_interpolation() {
    assert_eq!(out("let n = 3\nlet s = \"x\"\nprint(\"n = {n}, s = {s}, sum = {n + 4}\")\nprint(\"brace: \\{ok\\}\")"), "n = 3, s = x, sum = 7\nbrace: {ok}\n");
}

#[test]
fn tuples_arrays_and_vectors_print() {
    assert_eq!(out("print((1, \"a\", 2.5))\nprint([1, 2, 3])\nprint(vec[1.5, 2.5])\nprint(())\nprint((1,))"), "(1, a, 2.5)\n[1, 2, 3]\n[1.5, 2.5]\n()\n(1,)\n");
}

// ------------------------------------------------------------------ overflow and panics

#[test]
fn integer_overflow_panics_at_the_static_width() {
    panics("let x: i32 = 2147483647\nlet y = x + 1\nprint(y)", "integer overflow");
    panics("let x: u8 = 0\nlet y = x - 1\nprint(y)", "integer overflow");
    panics("let x: i64 = 9223372036854775807\nprint(x * 2)", "integer overflow");
    assert_eq!(out("let x: u8 = 200\nlet y = x as u32 + 100\nprint(y)"), "300\n");
}

#[test]
fn abs_of_the_most_negative_integer_overflows() {
    panics("let x: i8 = -128\nprint(abs(x))", "integer overflow");
    assert_eq!(out("let x: i8 = -127\nprint(abs(x))\nprint(abs(-5))"), "127\n5\n");
}

#[test]
fn map_assignment_inserts_and_compound_assignment_starts_at_zero() {
    let src = "use std.map.Map\nfn main() {\n let mut m = Map<str, i64>.new()\n m[\"a\"] = 1\n m[\"b\"] = 2\n m[\"a\"] = 10\n m[\"c\"] += 5\n m[\"b\"] -= 7\n print(m[\"a\"])\n print(m[\"b\"])\n print(m[\"c\"])\n let mut t = Map<(i64, i64), i64>.new()\n t[(1, 2)] = 3\n print(t[(1, 2)])\n}";
    assert_eq!(out_prog(src), "10\n-5\n5\n3\n");
    let src = "use std.map.Map\nfn main() {\n let m = Map<str, i64>.new()\n print(m[\"x\"])\n}";
    let (_, outcome) = run(src);
    assert!(matches!(outcome, Outcome::Panic(m) if m.contains("key not found")));
}

#[test]
fn a_tasks_output_appears_when_it_is_joined() {
    let src = "fn main() {\n let h = spawn {\n print(\"in task\")\n 5\n }\n print(\"main\")\n let g = spawn {\n print(\"second\")\n 6\n }\n match h.join() {\n Ok(v) => print(v)\n Err(_) => print(\"failed\")\n }\n match g.join() {\n Ok(v) => print(v)\n Err(_) => print(\"failed\")\n }\n}";
    assert_eq!(out_prog(src), "main\nin task\n5\nsecond\n6\n");
    // a task that panics still shows what it printed before, and joining reports the failure
    let src = "fn main() {\n let h = spawn {\n print(\"before\")\n panic(\"boom\")\n 1\n }\n match h.join() {\n Ok(_) => print(\"ok\")\n Err(_) => print(\"task failed\")\n }\n}";
    assert_eq!(out_prog(src), "before\ntask failed\n");
}

#[test]
fn huge_tensors_and_arrays_panic_instead_of_exhausting_memory() {
    panics_prog("fn main() {\n let a = Tensor<f64, [2147483647]>.ones()\n print(a.len)\n}", "a tensor can have at most 268435456 elements");
    panics_prog("fn main() {\n let n = 100000\n let a = Tensor<f64, [_, _]>.zeros(n, n)\n print(a.len)\n}", "a tensor can have at most");
    panics_prog("fn main() {\n let n = 100000\n let a = Tensor<f64, [_, _]>.ones(n, 1)\n let b = Tensor<f64, [_, _]>.ones(1, n)\n print((a @ b).len)\n}", "a tensor can have at most");
    panics_prog("fn main() {\n let n = 2147483647\n let a = [0; n]\n print(a.len)\n}", "an array can have at most");
}

#[test]
fn division_by_zero_panics() {
    panics("let z = 0\nprint(1 / z)", "division by zero");
    assert_eq!(out("print(1.0 / 0.0)"), "inf\n");
}

#[test]
fn index_out_of_bounds_panics() {
    panics("let v = vec[1, 2, 3]\nprint(v[5])", "index out of bounds: the length is 3 but the index is 5");
    panics("let a = [1, 2]\nlet i = 2\nprint(a[i])", "index out of bounds");
}

#[test]
fn explicit_panics_and_assertions() {
    panics("panic(\"boom\")", "boom");
    panics("assert(1 > 2)", "assertion failed");
    panics("assert_eq(1 + 1, 3)", "left != right");
    assert_eq!(out("assert(1 < 2)\nassert_eq(2 + 2, 4)\nprint(\"fine\")"), "fine\n");
}

#[test]
fn output_before_a_panic_is_kept() {
    let (text, outcome) = run("fn main() {\n print(\"before\")\n panic(\"stop\")\n print(\"after\")\n}");
    assert_eq!(text, "before\n");
    assert_eq!(outcome, Outcome::Panic("stop".into()));
}

#[test]
fn runaway_recursion_is_a_panic_not_a_crash() {
    let (_, outcome) = run("fn f(n: i64) -> i64 { f(n + 1) + 1 }\nfn main() { print(f(0)) }");
    match outcome {
        Outcome::Panic(m) => assert!(m.contains("stack overflow"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn deep_but_finite_recursion_works() {
    assert_eq!(out_prog("fn sum(n: i64) -> i64 { if n == 0 { 0 } else { n + sum(n - 1) } }\nfn main() { print(sum(2000)) }"), "2001000\n");
}

// ------------------------------------------------------------------ control flow

#[test]
fn loops() {
    assert_eq!(out("let mut t = 0\nfor i in 1..=10 { t += i }\nprint(t)"), "55\n");
    assert_eq!(out("let mut n = 0\nwhile n < 5 { n += 2 }\nprint(n)"), "6\n");
    assert_eq!(out("let mut i = 0\nlet r = loop {\n i += 3\n if i > 10 { break i * 2 }\n}\nprint(r)"), "24\n");
}

#[test]
fn break_and_continue() {
    assert_eq!(out("let mut s = 0\nfor i in 0..10 {\n if i % 2 == 0 { continue }\n if i > 7 { break }\n s += i\n}\nprint(s)"), "16\n");
}

#[test]
fn for_with_a_where_guard() {
    assert_eq!(out("let mut s = 0\nfor i in 0..10 where i % 3 == 0 { s += i }\nprint(s)"), "18\n");
}

#[test]
fn if_is_an_expression() {
    assert_eq!(out("let x = if 3 > 2 { \"yes\" } else { \"no\" }\nprint(x)\nlet y = if false { 1 } else if true { 2 } else { 3 }\nprint(y)"), "yes\n2\n");
}

#[test]
fn match_on_values_ranges_and_guards() {
    let src = "fn grade(n: i64) -> str {\n match n {\n 90..=100 => \"A\"\n 80..=89 => \"B\"\n x if x < 0 => \"invalid\"\n 0 | 1 | 2 => \"tiny\"\n _ => \"other\"\n }\n}\nfn main() {\n print(grade(95))\n print(grade(85))\n print(grade(-4))\n print(grade(1))\n print(grade(50))\n}";
    assert_eq!(out_prog(src), "A\nB\ninvalid\ntiny\nother\n");
}

#[test]
fn match_on_tuples() {
    let src = "fn main() {\n for i in 1..=15 {\n match (i % 3, i % 5) {\n (0, 0) => print(\"FizzBuzz\")\n (0, _) => print(\"Fizz\")\n (_, 0) => print(\"Buzz\")\n _ => print(i)\n }\n }\n}";
    assert_eq!(out_prog(src), "1\n2\nFizz\n4\nBuzz\nFizz\n7\n8\nFizz\nBuzz\n11\nFizz\n13\n14\nFizzBuzz\n");
}

// ------------------------------------------------------------------ functions and values

#[test]
fn recursion() {
    assert_eq!(out_prog("fn fib(n: i64) -> i64 { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }\nfn main() { print(fib(20)) }"), "6765\n");
    assert_eq!(out_prog("fn fact(n: i64) -> i64 { if n <= 1 { 1 } else { n * fact(n - 1) } }\nfn main() { print(fact(20)) }"), "2432902008176640000\n");
    panics_prog("fn fact(n: i64) -> i64 { if n <= 1 { 1 } else { n * fact(n - 1) } }\nfn main() { print(fact(21)) }", "integer overflow");
}

fn panics_prog(src: &str, needle: &str) {
    let (_, outcome) = run(src);
    match outcome {
        Outcome::Panic(m) => assert!(m.contains(needle), "expected a panic containing {needle:?}, got {m:?}"),
        other => panic!("expected a panic containing {needle:?}, got {other:?}"),
    }
}

#[test]
fn nested_functions_and_early_return() {
    let src = "fn find(v: [i64], x: i64) -> i64 {\n for i in 0..v.len {\n if v[i] == x { return i as i64 }\n }\n -1\n}\nfn main() {\n print(find([5, 6, 7], 7))\n print(find([5, 6, 7], 9))\n}";
    assert_eq!(out_prog(src), "2\n-1\n");
}

#[test]
fn shadowing() {
    assert_eq!(out("let x = 1\nlet x = x + 10\n{\n let x = x * 2\n print(x)\n}\nprint(x)"), "22\n11\n");
}

#[test]
fn closures_capture_and_mutate() {
    assert_eq!(out("let k = 10\nlet add = |x: i64| x + k\nprint(add(5))"), "15\n");
    assert_eq!(out("let mut n = 0\nlet inc = || n += 1\ninc()\ninc()\ninc()\nprint(n)"), "3\n");
    assert_eq!(out_prog("fn apply(f: fn(i64) -> i64, x: i64) -> i64 { f(x) }\nfn main() {\n print(apply(|x| x * x, 7))\n}"), "49\n");
}

#[test]
fn functions_are_values() {
    assert_eq!(out_prog("fn dbl(x: i64) -> i64 { x * 2 }\nfn apply(f: fn(i64) -> i64, x: i64) -> i64 { f(x) }\nfn main() {\n print(apply(dbl, 21))\n let g = dbl\n print(g(4))\n}"), "42\n8\n");
}

// ------------------------------------------------------------------ value semantics

#[test]
fn copy_aggregates_are_independent_copies() {
    // an array of Copy values is copied by `let`
    assert_eq!(out("let a = [1, 2, 3]\nlet mut b = a\nb[0] = 99\nprint(a[0])\nprint(b[0])"), "1\n99\n");
    // so is a tuple
    assert_eq!(out("let a = (1, 2)\nlet mut b = a\nb.0 = 7\nprint(a.0)\nprint(b.0)"), "1\n7\n");
    // and a struct with an `impl Copy`
    let src = "struct P { x: i64 }\nimpl Copy for P {}\nfn main() {\n let a = P(1)\n let mut b = a\n b.x = 5\n print(a.x)\n print(b.x)\n}";
    assert_eq!(out_prog(src), "1\n5\n");
}

#[test]
fn passing_a_copy_value_to_an_own_parameter_copies_it() {
    let src = "struct P { x: i64 }\nimpl Copy for P {}\nfn bump(own p: P) -> i64 {\n p.x = p.x + 100\n p.x\n}\nfn main() {\n let a = P(1)\n print(bump(a))\n print(a.x)\n}";
    assert_eq!(out_prog(src), "101\n1\n");
}

#[test]
fn moving_a_vec_does_not_copy_it() {
    // after the move the new name sees pushes made through it
    assert_eq!(out("let mut a = vec[1, 2]\nlet mut b = a\nb.push(3)\nprint(b)"), "[1, 2, 3]\n");
}

#[test]
fn mut_parameters_write_through_to_the_caller() {
    assert_eq!(out_prog("fn inc(x: mut i64) { x += 1 }\nfn main() {\n let mut n = 5\n inc(mut n)\n inc(mut n)\n print(n)\n}"), "7\n");
    assert_eq!(out_prog("fn grow(v: mut Vec<i64>) { v.push(9) }\nfn main() {\n let mut v = vec[1]\n grow(mut v)\n grow(mut v)\n print(v)\n}"), "[1, 9, 9]\n");
    // assigning to a whole `mut` parameter replaces the caller's value
    assert_eq!(out_prog("fn reset(v: mut Vec<i64>) { v = vec[0] }\nfn main() {\n let mut v = vec[1, 2, 3]\n reset(mut v)\n print(v)\n}"), "[0]\n");
}

#[test]
fn mut_parameters_can_be_handed_on() {
    let src = "fn inner(x: mut i64) { x += 10 }\nfn outer(x: mut i64) {\n inner(mut x)\n inner(mut x)\n}\nfn main() {\n let mut n = 1\n outer(mut n)\n print(n)\n}";
    assert_eq!(out_prog(src), "21\n");
}

#[test]
fn mut_arguments_can_be_fields_and_elements() {
    let src = "struct S { n: i64, v: Vec<i64> }\nfn inc(x: mut i64) { x += 1 }\nfn main() {\n let mut s = S(0, vec[10, 20])\n inc(mut s.n)\n inc(mut s.v[1])\n print(s.n)\n print(s.v)\n}";
    assert_eq!(out_prog(src), "1\n[10, 21]\n");
}

#[test]
fn mut_self_methods_change_the_receiver() {
    let src = "struct C { n: i64 }\nimpl C {\n fn bump(mut self) { self.n += 1 }\n fn get(self) -> i64 { self.n }\n}\nfn main() {\n let mut c = C(0)\n c.bump()\n c.bump()\n c.bump()\n print(c.get())\n}";
    assert_eq!(out_prog(src), "3\n");
}

#[test]
fn for_over_mut_changes_the_elements() {
    assert_eq!(out("let mut v = vec[1, 2, 3]\nfor x in mut v { x *= 10 }\nprint(v)"), "[10, 20, 30]\n");
    let src = "struct B { x: i64 }\nfn main() {\n let mut bs = vec[B(1), B(2)]\n for b in mut bs { b.x += 5 }\n print(bs[0].x)\n print(bs[1].x)\n}";
    assert_eq!(out_prog(src), "6\n7\n");
}

#[test]
fn plain_for_reads_without_changing() {
    assert_eq!(out("let v = vec[1, 2, 3]\nlet mut t = 0\nfor x in v { t += x }\nprint(t)\nprint(v)"), "6\n[1, 2, 3]\n");
}

#[test]
fn slices_are_views_of_the_original() {
    let src = "fn zero_first(s: mut [i64]) { s[0] = 0 }\nfn main() {\n let mut v = vec[5, 6, 7, 8]\n let w = v[1..3]\n print(w)\n print(w.len)\n zero_first(mut v)\n print(v)\n}";
    assert_eq!(out_prog(src), "[6, 7]\n2\n[0, 6, 7, 8]\n");
    assert_eq!(out("let v = vec[1, 2, 3, 4, 5]\nprint(v[1..4])\nprint(v[0..0].len)"), "[2, 3, 4]\n0\n");
}

#[test]
fn array_repeat_and_nested_arrays() {
    assert_eq!(out("let mut a = [0; 4]\na[2] = 7\nprint(a)"), "[0, 0, 7, 0]\n");
    assert_eq!(out("let mut g = [[0; 2]; 2]\ng[0][1] = 5\nprint(g)"), "[[0, 5], [0, 0]]\n");
}

#[test]
fn field_assignment_and_nested_structs() {
    let src = "struct In { v: i64 }\nstruct Out { a: In, b: i64 }\nfn main() {\n let mut o = Out(In(1), 2)\n o.a.v = 10\n o.b += 5\n print(o.a.v)\n print(o.b)\n}";
    assert_eq!(out_prog(src), "10\n7\n");
}

// ------------------------------------------------------------------ strings and collections

#[test]
fn string_operations() {
    assert_eq!(out("let s = \"Hello World\"\nprint(s.lowercase())\nprint(s.uppercase())\nprint(s.len)"), "hello world\nHELLO WORLD\n11\n");
    assert_eq!(out("let mut s = String.from(\"ab\")\ns.push_str(\"cd\")\nprint(s)\nprint(s.len)"), "abcd\n4\n");
    assert_eq!(out("let words = \"  the  quick brown \".split_whitespace()\nprint(words)\nprint(words.len)"), "[the, quick, brown]\n3\n");
}

#[test]
fn vec_operations() {
    let body = "let mut v = Vec<i64>.new()\nv.push(3)\nv.push(1)\nv.push(2)\nprint(v)\nprint(v.len)\nmatch v.pop() {\n Some(x) => print(x)\n None => print(\"empty\")\n}\nprint(v)";
    assert_eq!(out(body), "[3, 1, 2]\n3\n2\n[3, 1]\n");
    assert_eq!(out("let mut v = Vec<i64>.new()\nmatch v.pop() {\n Some(x) => print(x)\n None => print(\"empty\")\n}"), "empty\n");
}

#[test]
fn higher_order_functions_on_collections() {
    assert_eq!(out("let v = vec[1, 2, 3, 4, 5, 6]\nlet sq = v.map(|x| x * x)\nprint(sq)\nlet ev = v.filter(|x| x % 2 == 0)\nprint(ev)\nprint(sq.sum())\nprint(sum(ev))"), "[1, 4, 9, 16, 25, 36]\n[2, 4, 6]\n91\n12\n");
}

#[test]
fn maps() {
    let src = "use std.map.Map\nfn main() {\n let mut m = Map<str, i64>.new()\n m[\"a\"] += 2\n m[\"b\"] += 5\n m[\"a\"] += 1\n print(m[\"a\"])\n print(m[\"b\"])\n print(m.contains(\"a\"))\n print(m.contains(\"z\"))\n for (k, v) in m.top(5) {\n print(\"{k}={v}\")\n }\n}";
    assert_eq!(out_prog(src), "3\n5\ntrue\nfalse\nb=5\na=3\n");
}

#[test]
fn reading_a_missing_map_key_panics() {
    panics_prog("use std.map.Map\nfn main() {\n let m = Map<str, i64>.new()\n print(m[\"nope\"])\n}", "key not found");
}

// ------------------------------------------------------------------ enums, options, errors

#[test]
fn enums_and_pattern_matching() {
    let src = "enum Shape {\n Circle(f64)\n Rect(f64, f64)\n Empty\n}\nfn area(s: Shape) -> f64 {\n match s {\n Circle(r) => 3.0 * r * r\n Rect(w, h) => w * h\n Empty => 0.0\n }\n}\nfn main() {\n print(area(Circle(2.0)))\n print(area(Rect(3.0, 4.0)))\n print(area(Empty))\n}";
    assert_eq!(out_prog(src), "12.0\n12.0\n0.0\n");
}

#[test]
fn options_and_matching_on_them() {
    let src = "fn first(v: [i64]) -> Option<i64> {\n if v.is_empty() { None } else { Some(v[0]) }\n}\nfn show(o: Option<i64>) -> str {\n match o {\n Some(x) if x > 5 => \"big\"\n Some(_) => \"small\"\n None => \"none\"\n }\n}\nfn main() {\n print(show(first([9, 1])))\n print(show(first([2, 1])))\n print(show(first([])))\n}";
    assert_eq!(out_prog(src), "big\nsmall\nnone\n");
}

#[test]
fn results_and_the_question_mark() {
    let src = "enum E {\n Neg\n Big\n}\nfn check(n: i64) -> Result<i64, E> {\n if n < 0 { return Err(Neg) }\n if n > 100 { return Err(Big) }\n Ok(n)\n}\nfn twice(n: i64) -> Result<i64, E> {\n let x = check(n)?\n Ok(x * 2)\n}\nfn show(r: Result<i64, E>) -> str {\n match r {\n Ok(_) => \"ok\"\n Err(Neg) => \"negative\"\n Err(Big) => \"too big\"\n }\n}\nfn main() {\n print(show(twice(5)))\n print(show(twice(-1)))\n print(show(twice(500)))\n match twice(21) {\n Ok(v) => print(v)\n Err(_) => print(0)\n }\n}";
    assert_eq!(out_prog(src), "ok\nnegative\ntoo big\n42\n");
}

#[test]
fn main_may_return_an_error() {
    let (text, outcome) = run("use std.fs\nfn main() -> Result<(), IoError> {\n print(\"start\")\n let t = fs.read(\"/definitely/not/here.txt\")?\n print(t)\n Ok(())\n}");
    assert_eq!(text, "start\n");
    match outcome {
        Outcome::Error(m) => assert!(m.contains("io error"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn file_round_trip() {
    let dir = std::env::temp_dir().join(format!("tau-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("note.txt");
    let src = format!(
        "use std.fs\nfn main() -> Result<(), IoError> {{\n fs.write(\"{p}\", \"hello file\")?\n print(fs.exists(\"{p}\"))\n let t = fs.read(\"{p}\")?\n print(t)\n Ok(())\n}}",
        p = path.display()
    );
    assert_eq!(out_prog(&src), "true\nhello file\n");
    std::fs::remove_dir_all(&dir).ok();
}

// ------------------------------------------------------------------ traits, generics, operators

#[test]
fn generic_functions_dispatch_to_trait_impls() {
    let src = "trait Zero {\n fn zero() -> Self\n fn add(self, o: Self) -> Self\n}\nimpl Zero for f64 {\n fn zero() -> f64 { 0.0 }\n fn add(self, o: f64) -> f64 { self + o }\n}\nimpl Zero for i64 {\n fn zero() -> i64 { 0 }\n fn add(self, o: i64) -> i64 { self + o }\n}\nfn total<T: Zero>(xs: [T]) -> T {\n let mut acc = T.zero()\n for x in xs { acc = acc.add(x) }\n acc\n}\nfn main() {\n print(total([1.5, 2.5]))\n print(total([1, 2, 3]))\n}";
    assert_eq!(out_prog(src), "4.0\n6\n");
}

#[test]
fn trait_default_methods_use_the_implementing_type() {
    let src = "trait Named {\n fn name(self) -> str\n fn shout(self) -> String { self.name().uppercase() }\n}\nstruct Dog {}\nstruct Cat {}\nimpl Named for Dog {\n fn name(self) -> str { \"dog\" }\n}\nimpl Named for Cat {\n fn name(self) -> str { \"cat\" }\n}\nfn main() {\n print(Dog().shout())\n print(Cat().shout())\n}";
    assert_eq!(out_prog(src), "DOG\nCAT\n");
}

#[test]
fn operator_overloading() {
    let src = "struct V { x: f64, y: f64 }\nimpl Add for V {\n fn add(self, o: V) -> V { V(self.x + o.x, self.y + o.y) }\n}\nimpl Sub for V {\n fn sub(self, o: V) -> V { V(self.x - o.x, self.y - o.y) }\n}\nimpl Mul for V {\n fn mul(self, k: f64) -> V { V(self.x * k, self.y * k) }\n}\nimpl Mul for V {\n fn mul(self, o: V) -> f64 { self.x * o.x + self.y * o.y }\n}\nimpl Neg for V {\n fn neg(self) -> V { V(-self.x, -self.y) }\n}\nimpl Display for V {\n fn fmt(self) -> String { String.from(\"V\") }\n}\nfn main() {\n let a = V(1.0, 2.0)\n let b = V(3.0, 5.0)\n let c = a + b\n print(c.x)\n print(c.y)\n let d = (b - a) * 2.0\n print(d.x)\n print(a * b)\n let n = -a\n print(n.y)\n let mut e = a\n e += b\n print(e.x)\n print(e)\n}";
    assert_eq!(out_prog(src), "4.0\n7.0\n4.0\n13.0\n-2.0\n4.0\nV\n");
}

#[test]
fn user_eq_ord_and_display() {
    let src = "struct P { n: i64 }\nimpl Eq for P {\n fn eq(self, o: P) -> bool { self.n == o.n }\n}\nimpl Ord for P {\n fn lt(self, o: P) -> bool { self.n < o.n }\n}\nimpl Display for P {\n fn fmt(self) -> String { String.from(\"P!\") }\n}\nfn biggest<T: Ord>(own a: T, own b: T) -> T {\n if a < b { b } else { a }\n}\nfn main() {\n print(P(1) == P(1))\n print(P(1) != P(2))\n print(P(1) < P(2))\n print(P(3) > P(2))\n print(P(3) <= P(2))\n print(P(3) >= P(3))\n print(biggest(P(4), P(9)))\n print(biggest(7, 3))\n}";
    assert_eq!(out_prog(src), "true\ntrue\ntrue\ntrue\nfalse\ntrue\nP!\n7\n");
}

#[test]
fn num_bounded_generics() {
    let src = "fn sq<T: Num>(a: T) -> T { a * a }\nfn main() {\n print(sq(7))\n print(sq(1.5))\n print(sq(3u8))\n}";
    assert_eq!(out_prog(src), "49\n2.25\n9\n");
}

#[test]
fn generic_overflow_uses_the_instantiated_width() {
    panics_prog("fn sq<T: Num>(a: T) -> T { a * a }\nfn main() {\n print(sq(200u8))\n}", "integer overflow");
}

#[test]
fn generic_structs() {
    let src = "struct Pair<A, B> { a: A, b: B }\nfn swap<A, B>(own p: Pair<A, B>) -> Pair<B, A> { Pair(p.b, p.a) }\nfn main() {\n let p = swap(Pair(1, \"one\"))\n print(p.a)\n print(p.b)\n}";
    assert_eq!(out_prog(src), "one\n1\n");
}

// ------------------------------------------------------------------ tensors

#[test]
fn matrix_product_and_elementwise_ops() {
    // [[1, 2], [3, 4]] @ [[5, 6], [7, 8]] = [[19, 22], [43, 50]]
    let src = "fn main() {\n let a = Tensor<f64, [2, 2]>.ones()\n let mut b = a * 2.0\n print(a + b)\n print(b @ b)\n}";
    assert_eq!(out_prog(src), "[[3.0, 3.0], [3.0, 3.0]]\n[[8.0, 8.0], [8.0, 8.0]]\n");
}

#[test]
fn tensor_indexing_and_assignment() {
    let src = "fn main() {\n let mut a = Tensor<f64, [2, 3]>.zeros()\n a[0, 2] = 5.0\n a[1, 0] = 7.0\n print(a)\n print(a[0, 2])\n print(trace(Tensor<f64, [2, 2]>.ones()))\n}";
    assert_eq!(out_prog(src), "[[0.0, 0.0, 5.0], [7.0, 0.0, 0.0]]\n5.0\n2.0\n");
}

#[test]
fn matrix_vector_and_dot() {
    let src = "fn main() {\n let a = Tensor<f64, [2, 3]>.ones()\n let v = Tensor<f64, [3]>.ones()\n print(a @ v)\n print(v @ v)\n}";
    assert_eq!(out_prog(src), "[3.0, 3.0]\n3.0\n");
}

#[test]
fn shape_mismatches_are_caught() {
    panics_prog("fn main() {\n let a = Tensor<f64, [_, _]>.zeros(2, 3)\n let b = Tensor<f64, [_, _]>.zeros(2, 3)\n print(a @ b)\n}", "shape mismatch");
    panics_prog("fn main() {\n let a = Tensor<f64, [_, _]>.zeros(2, 3)\n print(a[5, 0])\n}", "out of bounds");
}

#[test]
fn linear_algebra() {
    // det [[1,2],[3,4]] = -2;  inv = [[-2, 1], [1.5, -0.5]] (up to rounding);  transpose swaps the off-diagonals
    let src = "use math.linalg\nfn main() {\n let mut m = Tensor<f64, [2, 2]>.zeros()\n m[0, 0] = 1.0\n m[0, 1] = 2.0\n m[1, 0] = 3.0\n m[1, 1] = 4.0\n print(linalg.det(m))\n let i = linalg.inv(m)\n print(round(i[0, 0] * 1000000.0) / 1000000.0)\n print(round(i[0, 1] * 1000000.0) / 1000000.0)\n print(round(i[1, 0] * 1000000.0) / 1000000.0)\n print(round(i[1, 1] * 1000000.0) / 1000000.0)\n print(linalg.transpose(m))\n}";
    assert_eq!(out_prog(src), "-2.0\n-2.0\n1.0\n1.5\n-0.5\n[[1.0, 3.0], [2.0, 4.0]]\n");
}

#[test]
fn a_singular_matrix_cannot_be_inverted() {
    panics_prog("use math.linalg\nfn main() {\n let m = Tensor<f64, [2, 2]>.zeros()\n print(linalg.inv(m))\n}", "singular");
}

#[test]
fn eigenvalues_of_a_symmetric_matrix() {
    // [[2, 1], [1, 2]] has eigenvalues 1 and 3
    let src = "use math.linalg\nfn main() {\n let mut m = Tensor<f64, [2, 2]>.zeros()\n m[0, 0] = 2.0\n m[1, 1] = 2.0\n m[0, 1] = 1.0\n m[1, 0] = 1.0\n let (vals, vecs) = linalg.eig(m)\n print(round(vals[0] * 1000.0) / 1000.0)\n print(round(vals[1] * 1000.0) / 1000.0)\n}";
    assert_eq!(out_prog(src), "1.0\n3.0\n");
}

#[test]
fn fft_of_an_impulse_is_flat() {
    let src = "use math.fft\nfn main() {\n let mut x = Tensor<f64, [4]>.zeros()\n x[0] = 1.0\n let spectrum = fft.fft(x)\n print(spectrum)\n let back = fft.ifft(spectrum)\n print(back)\n}";
    assert_eq!(out_prog(src), "[1.0+0.0i, 1.0+0.0i, 1.0+0.0i, 1.0+0.0i]\n[1.0, 0.0, 0.0, 0.0]\n");
}

// ------------------------------------------------------------------ tasks and parallel constructs

#[test]
fn spawn_and_join() {
    assert_eq!(out_prog("fn main() {\n let h = spawn { 6 * 7 }\n match h.join() {\n Ok(v) => print(v)\n Err(_) => print(\"failed\")\n }\n}"), "42\n");
}

#[test]
fn a_panicking_task_is_reported_by_join() {
    let src = "fn main() {\n let h = spawn {\n panic(\"inside\")\n 1\n }\n match h.join() {\n Ok(v) => print(v)\n Err(_) => print(\"task failed\")\n }\n print(\"main continues\")\n}";
    assert_eq!(out_prog(src), "task failed\nmain continues\n");
}

#[test]
fn par_loops_compute_what_the_sequential_ones_do() {
    let src = "fn main() {\n let a = vec[1.0, 2.0, 3.0, 4.0]\n let mut out = vec[0.0, 0.0, 0.0, 0.0]\n par for i in 0..a.len {\n out[i] = a[i] * a[i]\n }\n print(out)\n print(par sum(i in 0..a.len) a[i])\n print(par max(i in 0..a.len) a[i])\n print(par any(i in 0..a.len) a[i] > 3.5)\n print(par all(i in 0..a.len) a[i] > 1.5)\n}";
    assert_eq!(out_prog(src), "[1.0, 4.0, 9.0, 16.0]\n10.0\n4.0\ntrue\nfalse\n");
}

#[test]
fn par_chunks_gives_disjoint_slices() {
    let src = "fn main() {\n let mut v = vec[1, 2, 3, 4, 5]\n par for c in par_chunks(mut v, 2) {\n c[0] = 0\n }\n print(v)\n}";
    assert_eq!(out_prog(src), "[0, 2, 0, 4, 0]\n");
}

#[test]
fn on_runs_locally_for_now() {
    assert_eq!(out_prog("fn main() {\n let r = on local { 1 + 2 }\n print(r)\n}"), "3\n");
}

// ------------------------------------------------------------------ nothing runs unless it checks

#[test]
fn rejected_programs_do_not_run() {
    let cap = Capture(Arc::new(Mutex::new(Vec::new())));
    let r = tau::run_source("fn main() {\n print(\"should not print\")\n let a = vec[1]\n let b = a\n print(a.len)\n}", Box::new(cap.clone())).unwrap();
    assert!(!r.diagnostics.is_empty());
    assert!(r.outcome.is_none());
    assert!(cap.0.lock().unwrap().is_empty());
}

// ------------------------------------------------------------------ float formatting and reductions

#[test]
fn single_precision_floats_print_as_single_precision() {
    assert_eq!(out("let x: f32 = 0.1\nprint(x)\nlet z: f32 = 1.0 / 3.0\nprint(z)\nprint(1.0 / 3.0)"), "0.1\n0.33333334\n0.3333333333333333\n");
    assert_eq!(out("let x: f32 = 0.1\nprint(vec[x, 0.2f32])\nprint((x, 7))\nprint(\"{x}\")"), "[0.1, 0.2]\n(0.1, 7)\n0.1\n");
    assert_eq!(out("let big: f32 = 16777216.0\nprint(big + 1.0)"), "16777216.0\n");
}

#[test]
fn whole_floats_always_end_in_point_zero() {
    assert_eq!(out("print(2.0)\nprint(1.0e10)\nprint(1.0e16)\nprint(1.0e20)\nprint(-3.0)"), "2.0\n10000000000.0\n10000000000000000.0\n100000000000000000000.0\n-3.0\n");
}

#[test]
fn small_floats_never_use_scientific_notation() {
    assert_eq!(out("print(0.0000001)\nprint(0.1 + 0.2)\nprint(1.5e-5)"), "0.0000001\n0.30000000000000004\n0.000015\n");
}

#[test]
fn par_sum_is_defined_by_fixed_blocks_of_4096() {
    // One huge value then many ones. A plain left-to-right sum loses every `+ 1.0`
    // (1e16 + 1.0 rounds back to 1e16); the blocked sum adds the ones among themselves
    // first, so they survive: block 0 = 1e16, block 1 = 4096, block 2 = 1, combined in order.
    let src = "fn v(i: usize) -> f64 { if i == 0 { 10000000000000000.0 } else { 1.0 } }\n\
               fn main() {\n\
                 print(par sum(i in 0..8193) v(i))\n\
                 let mut s = 0.0\n\
                 for i in 0..8193 { s += v(i) }\n\
                 print(s)\n\
               }";
    assert_eq!(out_prog(src), "10000000000004096.0\n10000000000000000.0\n");
}

#[test]
fn par_sum_of_fewer_than_one_block_is_a_plain_sum() {
    let src = "fn main() {\n print(par sum(i in 0..10) i as f64 * 0.1)\n let mut s = 0.0\n for i in 0..10 { s += i as f64 * 0.1 }\n print(s)\n}";
    let text = out_prog(src);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], lines[1], "{text}");
}

#[test]
fn par_min_max_any_all_do_not_depend_on_blocking() {
    assert_eq!(out("print(par max(i in 0..10000) (i * 7919) % 10007)\nprint(par min(i in 1..10000) (i * 7919) % 10007)\nprint(par any(i in 0..10000) i == 9999)\nprint(par all(i in 0..10000) i < 9999)"), "10006\n1\ntrue\nfalse\n");
}
