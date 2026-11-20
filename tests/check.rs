//! Type-checker tests: inferred types for accepted programs, and the exact
//! diagnostics for rejected ones.

use tau::check_source;
use tau::CheckResult;

fn run(src: &str) -> CheckResult {
    match check_source(src) {
        Ok(r) => r,
        Err(d) => panic!("parse error:\n{}", d.render(src, "<test>")),
    }
}

fn render(src: &str, r: &CheckResult) -> String {
    r.diagnostics.iter().map(|d| d.render(src, "<test>")).collect::<Vec<_>>().join("\n")
}

/// The program must check with no errors.
fn ok(src: &str) -> CheckResult {
    let r = run(src);
    assert!(r.diagnostics.is_empty(), "unexpected errors:\n{}", render(src, &r));
    r
}

/// The inferred type of the first binding called `name`.
fn ty(src: &str, name: &str) -> String {
    ok(src).binding_ty(name).unwrap_or_else(|| panic!("no binding `{name}`"))
}

/// Check a `main` body: `ty_in_main("let x = 1", "x")`.
fn m(body: &str, name: &str) -> String {
    ty(&format!("fn main() {{\n{body}\n}}"), name)
}

/// The program must produce an error whose message contains `needle`.
fn err(src: &str, needle: &str) -> CheckResult {
    let r = run(src);
    let msgs: Vec<&str> = r.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert!(
        msgs.iter().any(|m| m.contains(needle)),
        "expected an error containing {needle:?}, got {msgs:?}\n{}",
        render(src, &r)
    );
    r
}

/// Like `err`, for a snippet that goes inside `fn main() { ... }`.
fn em(body: &str, needle: &str) -> CheckResult {
    err(&format!("fn main() {{\n{body}\n}}"), needle)
}

fn count(src: &str) -> usize {
    run(src).diagnostics.len()
}

// ------------------------------------------------------------------ literals and defaults

#[test]
fn integer_literals_default_to_i64() {
    assert_eq!(m("let x = 5", "x"), "i64");
}

#[test]
fn integer_literals_must_fit_their_type() {
    em("let a: i8 = 200", "the literal `200` does not fit in `i8`");
    em("let a: u8 = 256", "does not fit in `u8`");
    em("let a: i8 = -129", "does not fit in `i8`");
    em("let a = 9223372036854775808", "does not fit in `i64`");
    em("let a: u8 = 1\nlet b = a + 300", "does not fit in `u8`");
    // the extreme values themselves are fine, and so is negating a literal
    ok("fn main() {\n let a: i8 = -128\n let b: i8 = 127\n let c: u8 = 255\n let d: i64 = -9223372036854775808\n let e: u128 = 340282366920938463463374607431768211455\n let f: i128 = -170141183460469231731687303715884105728\n}");
}

#[test]
fn str_and_slices_are_copy() {
    // the spec: numbers, bool, char, `str` and slices are `Copy`
    ok("fn id<T: Copy>(x: T) -> T { x }\nfn main() {\n let a = id(\"s\")\n let v = vec[1, 2]\n let s = id(v[0..1])\n}");
}

#[test]
fn arrays_have_a_size_limit() {
    em("let a = [0; 16777217]", "an array can have at most 16777216 elements");
    em("let a: [i64; 100000000] = [0; 3]", "an array can have at most");
    ok("fn main() {\n let a = [0; 16777216]\n}");
}

#[test]
fn float_literals_default_to_f64() {
    assert_eq!(m("let x = 2.5", "x"), "f64");
}

#[test]
fn suffixes_and_annotations_fix_the_type() {
    assert_eq!(m("let x = 5u8", "x"), "u8");
    assert_eq!(m("let x: u16 = 5", "x"), "u16");
    assert_eq!(m("let x = 1.5f32", "x"), "f32");
    assert_eq!(m("let x: f32 = 1.5", "x"), "f32");
}

#[test]
fn integer_literal_can_be_a_float_but_not_the_reverse() {
    assert_eq!(m("let x: f64 = 1", "x"), "f64");
    em("let x: i64 = 1.5", "mismatched types");
}

#[test]
fn later_use_decides_the_literal_type() {
    assert_eq!(m("let x = 1\nlet y: usize = x", "x"), "usize");
    assert_eq!(m("let a = [1, 2, 3]\nfor i in 0..3 { let v = a[i] }", "i"), "usize");
}

#[test]
fn literal_with_a_float_operand_becomes_float() {
    assert_eq!(m("let x = 2 * 1.5", "x"), "f64");
    assert_eq!(m("let y = 3.0\nlet x = 2 * y", "x"), "f64");
}

#[test]
fn complex_literals() {
    assert_eq!(m("let z = 3 + 4i", "z"), "c128");
}

#[test]
fn compound_types() {
    assert_eq!(m("let t = (1, \"a\", 2.0)", "t"), "(i64, str, f64)");
    assert_eq!(m("let a = [1, 2, 3]", "a"), "[i64; 3]");
    assert_eq!(m("let a = [0.0; 4]", "a"), "[f64; 4]");
    assert_eq!(m("let v = vec[1.0, 2.0]", "v"), "Vec<f64>");
    assert_eq!(m("let u = ()", "u"), "()");
    assert_eq!(m("let s = \"hi\"", "s"), "str");
    assert_eq!(m("let c = 'x'", "c"), "char");
    assert_eq!(m("let b = 1 < 2", "b"), "bool");
}

// ------------------------------------------------------------------ operators and casts

#[test]
fn arithmetic_requires_matching_types() {
    em("let a: i64 = 1\nlet b: f64 = 2.0\nlet c = a + b", "mismatched types");
    em("let a: i32 = 1\nlet b: i64 = 2\nlet c = a * b", "mismatched types");
}

#[test]
fn casts() {
    assert_eq!(m("let n: usize = 3\nlet x = n as f64", "x"), "f64");
    assert_eq!(m("let x = 3.7 as i32", "x"), "i32");
    assert_eq!(m("let c = 65u8 as char", "c"), "char");
    assert_eq!(m("let n = 'a' as u32", "n"), "u32");
    em("let x = \"s\" as i32", "cannot cast");
    em("let x = 5 as bool", "cannot cast");
}

#[test]
fn power_and_bitwise() {
    assert_eq!(m("let x = 2.0\nlet y = x^2", "y"), "f64");
    assert_eq!(m("let a = 5u32 ^^ 3u32", "a"), "u32");
    assert_eq!(m("let a = 1u32 << 3u32", "a"), "u32");
    em("let a = 1.5 ^^ 2.5", "only works on integers");
}

#[test]
fn comparison_and_logic() {
    assert_eq!(m("let a = 1 == 2 && 3 < 4", "a"), "bool");
    em("let a = 1 && true", "mismatched types");
    assert_eq!(m("let a = 1 == 2.5", "a"), "bool"); // an integer literal may be a float
    em("let n: i64 = 1\nlet a = n == 2.5", "mismatched types");
    em("let a = \"s\" == 1", "mismatched types");
}

#[test]
fn unary_operators() {
    assert_eq!(m("let x = -5", "x"), "i64");
    assert_eq!(m("let b = !true", "b"), "bool");
    em("let b = !1", "mismatched types");
    em("let b = -true", "cannot negate");
}

// ------------------------------------------------------------------ functions and generics

#[test]
fn function_calls_check_arguments() {
    let src = "fn f(x: f64) -> f64 { x }\nfn main() {\n let a = f(1.0)\n}";
    assert_eq!(ty(src, "a"), "f64");
    err("fn f(x: f64) -> f64 { x }\nfn main() { f(1, 2) }", "takes 1 argument, but 2 were supplied");
    err("fn f(x: f64) -> f64 { x }\nfn main() { f() }", "takes 1 argument, but 0 were supplied");
    err("fn f(x: f64) -> f64 { x }\nfn main() { f(\"s\") }", "mismatched types");
}

#[test]
fn return_type_is_checked() {
    err("fn f() -> i64 { \"s\" }", "mismatched types");
    err("fn f() -> i64 { }", "mismatched types");
    ok("fn f() -> i64 { return 1 }");
    err("fn f() -> i64 { return \"s\" }", "mismatched types");
}

#[test]
fn unit_functions_may_end_in_any_expression() {
    ok("fn g() -> i64 { 1 }\nfn f() { g() }");
}

#[test]
fn generic_functions_are_instantiated_per_call() {
    let src = "fn id<T>(x: T) -> T { x }\nfn main() {\n let a = id(5)\n let b = id(\"s\")\n let c = id(2.5)\n}";
    assert_eq!(ty(src, "a"), "i64");
    assert_eq!(ty(src, "b"), "str");
    assert_eq!(ty(src, "c"), "f64");
}

#[test]
fn explicit_type_arguments() {
    let src = "fn id<T>(x: T) -> T { x }\nfn main() {\n let a = id<u8>(5)\n}";
    assert_eq!(ty(src, "a"), "u8");
    err("fn id<T>(x: T) -> T { x }\nfn main() { id<u8, i32>(5) }", "type argument");
}

#[test]
fn trait_bounds_are_enforced_at_calls() {
    let base = "trait Number {\n fn zero() -> Self\n}\nimpl Number for f64 {\n fn zero() -> f64 { 0.0 }\n}\nfn z<T: Number>() -> T { T.zero() }\n";
    ok(&format!("{base}fn main() {{\n let a: f64 = z()\n}}"));
    err(&format!("{base}fn main() {{\n let a: i64 = z()\n}}"), "trait `Number` is not implemented for `i64`");
}

#[test]
fn generic_body_can_only_use_bounded_operations() {
    err("fn f<T>(a: T, b: T) -> T { a + b }", "cannot apply `+`");
    ok("fn f<T: Add>(a: T, b: T) -> T { a + b }");
    err("fn f<T>(a: T, b: T) -> bool { a < b }", "trait `Ord` is not implemented for `T`");
    ok("fn f<T: Ord>(a: T, b: T) -> bool { a < b }");
}

#[test]
fn num_bounded_parameters_support_arithmetic() {
    ok("fn f<T: Num>(a: T, b: T) -> T { a * b + a - b }");
    ok("fn f<T: Float>(a: T) -> T { -a / a }");
    ok("fn f<T: Int>(a: T, b: T) -> T { a ^^ b }");
    err("fn f<T: Num>(a: T, b: T) -> T { a ^^ b }", "only works on integers");
    err("fn f<T: Num>(a: T) -> T { a * 2 }", "mismatched types"); // a literal is not a `T`
    ok("fn main() {\n let x = (|a: f64| a)(1.0)\n}");
}

#[test]
fn generic_structs() {
    let src = "struct Pair<A, B> { a: A, b: B }\nfn main() {\n let p = Pair(1, \"x\")\n let q = p.b\n let r = p.a\n}";
    assert_eq!(ty(src, "p"), "Pair<i64, str>");
    assert_eq!(ty(src, "q"), "str");
    assert_eq!(ty(src, "r"), "i64");
}

#[test]
fn nested_functions() {
    ok("fn main() {\n fn helper(x: i64) -> i64 { x + 1 }\n let a = helper(2)\n}");
}

#[test]
fn recursion_and_mutual_recursion() {
    ok("fn fact(n: i64) -> i64 { if n <= 1 { 1 } else { n * fact(n - 1) } }");
    ok("fn even(n: i64) -> bool { if n == 0 { true } else { odd(n - 1) } }\nfn odd(n: i64) -> bool { if n == 0 { false } else { even(n - 1) } }");
}

// ------------------------------------------------------------------ structs, enums, methods

const VEC3: &str = "struct V { x: f64, y: f64 }\nimpl V {\n fn len2(self) -> f64 { self.x * self.x + self.y * self.y }\n fn origin() -> V { V(0.0, 0.0) }\n fn nudge(mut self, d: f64) { self.x += d }\n}\n";

#[test]
fn struct_construction_and_fields() {
    assert_eq!(ty(&format!("{VEC3}fn main() {{\n let v = V(1.0, 2.0)\n let a = v.x\n}}"), "a"), "f64");
    err(&format!("{VEC3}fn main() {{\n let v = V(1.0)\n}}"), "takes 2 arguments, but 1 was supplied");
    err(&format!("{VEC3}fn main() {{\n let v = V(1.0, 2.0)\n let a = v.z\n}}"), "no field `z` on type `V`");
    err(&format!("{VEC3}fn main() {{\n let v = V(1.0, \"s\")\n}}"), "mismatched types");
}

#[test]
fn methods_and_associated_functions() {
    let s = format!("{VEC3}fn main() {{\n let v = V.origin()\n let n = v.len2()\n}}");
    assert_eq!(ty(&s, "n"), "f64");
    err(&format!("{VEC3}fn main() {{\n let v = V.origin()\n v.origin()\n}}"), "no method named `origin`");
    err(&format!("{VEC3}fn main() {{\n V.len2()\n}}"), "no function named `len2`");
    err(&format!("{VEC3}fn main() {{\n let v = V.origin()\n v.nope()\n}}"), "no method named `nope`");
}

#[test]
fn mut_self_methods_need_a_mutable_receiver() {
    ok(&format!("{VEC3}fn main() {{\n let mut v = V.origin()\n v.nudge(1.0)\n}}"));
    err(&format!("{VEC3}fn main() {{\n let v = V.origin()\n v.nudge(1.0)\n}}"), "not declared `mut`");
}

#[test]
fn enums_and_match() {
    let src = "enum S {\n A(f64)\n B\n}\nfn f(s: S) -> f64 {\n match s {\n A(x) => x\n B => 0.0\n }\n}";
    ok(src);
    err("enum S {\n A\n B\n C\n}\nfn f(s: S) -> i64 {\n match s {\n A => 1\n B => 2\n }\n}", "missing `C`");
    err("enum S {\n A(f64)\n B\n}\nfn f(s: S) -> f64 {\n match s {\n A(x, y) => x\n B => 0.0\n }\n}", "has 1 field(s), but the pattern has 2");
    ok("enum S {\n A\n B\n}\nfn f(s: S) -> i64 {\n match s {\n A => 1\n _ => 2\n }\n}");
}

#[test]
fn match_arms_must_agree() {
    err("fn f(n: i64) -> i64 { match n {\n 0 => 1\n _ => \"s\"\n } }", "mismatched types");
    ok("fn f(n: i64) -> i64 { match n {\n 0 => return 1\n _ => 2\n } }");
}

#[test]
fn match_must_be_exhaustive() {
    err("fn f(n: i64) -> i64 { match n { 1 => 1 } }", "non-exhaustive");
    err("fn f(b: bool) -> i64 { match b { true => 1 } }", "non-exhaustive");
    ok("fn f(b: bool) -> i64 { match b {\n true => 1\n false => 0\n } }");
    ok("fn f(o: Option<i64>) -> i64 { match o {\n Some(x) => x\n None => 0\n } }");
    err("fn f(o: Option<i64>) -> i64 { match o { Some(x) => x } }", "missing `None`");
}

#[test]
fn match_patterns_bind_with_the_right_types() {
    let src = "fn f(o: Option<(i64, str)>) {\n match o {\n Some((n, s)) => print(n)\n None => print(0)\n }\n}";
    let r = ok(src);
    assert_eq!(r.binding_ty("n").unwrap(), "i64");
    assert_eq!(r.binding_ty("s").unwrap(), "str");
}

#[test]
fn or_patterns_must_bind_the_same_names() {
    ok("fn f(n: i64) -> i64 { match n {\n 1 | 2 => 1\n _ => 0\n } }");
    err("enum E {\n A(i64)\n B(i64)\n}\nfn f(e: E) -> i64 { match e {\n A(x) | B(y) => 1\n } }", "same variables");
}

// ------------------------------------------------------------------ operator overloading

const OPS: &str = "struct V { x: f64, y: f64 }\nimpl Add for V {\n fn add(self, o: V) -> V { V(self.x + o.x, self.y + o.y) }\n}\nimpl Mul for V {\n fn mul(self, k: f64) -> f64 { self.x * k }\n}\nimpl Mul for V {\n fn mul(self, o: V) -> V { o }\n}\n";

#[test]
fn operators_dispatch_to_impls_by_operand_type() {
    let s = format!("{OPS}fn main() {{\n let a = V(1.0, 2.0)\n let b = a + a\n let c = a * 2.0\n let d = a * a\n}}");
    assert_eq!(ty(&s, "b"), "V");
    assert_eq!(ty(&s, "c"), "f64");
    assert_eq!(ty(&s, "d"), "V");
}

#[test]
fn missing_operator_impls_are_reported() {
    err(&format!("{OPS}fn main() {{\n let a = V(1.0, 2.0)\n let b = a - a\n}}"), "cannot apply `-`");
    err(&format!("{OPS}fn main() {{\n let a = V(1.0, 2.0)\n let b = a + 1.0\n}}"), "cannot apply `+` to `V` and `{float}`");
}

#[test]
fn compound_assignment_uses_the_operator_impl() {
    ok(&format!("{OPS}fn main() {{\n let mut a = V(1.0, 2.0)\n a += a\n}}"));
}

#[test]
fn comparison_operators_need_eq_or_ord_impls() {
    err("struct P { x: i64 }\nfn main() { P(1) == P(1) }", "cannot compare");
    ok("struct P { x: i64 }\nimpl Eq for P {\n fn eq(self, o: P) -> bool { self.x == o.x }\n}\nfn main() { P(1) == P(1) }");
}

// ------------------------------------------------------------------ traits and impls

#[test]
fn impls_must_provide_all_methods() {
    err("trait T {\n fn a(self) -> i64\n fn b(self) -> i64\n}\nstruct S {}\nimpl T for S {\n fn a(self) -> i64 { 1 }\n}", "missing method `b`");
}

#[test]
fn trait_default_methods_need_no_impl() {
    ok("trait T {\n fn a(self) -> i64\n fn b(self) -> i64 { self.a() + 1 }\n}\nstruct S {}\nimpl T for S {\n fn a(self) -> i64 { 1 }\n}\nfn main() { S().b() }");
}

#[test]
fn impl_signatures_must_match_the_trait() {
    err("trait T {\n fn a(self, x: i64) -> i64\n}\nstruct S {}\nimpl T for S {\n fn a(self, x: f64) -> i64 { 1 }\n}", "has type `f64`, but trait `T` expects `i64`");
    err("trait T {\n fn a(self) -> i64\n}\nstruct S {}\nimpl T for S {\n fn a(self) -> f64 { 1.0 }\n}", "returns `f64`, but trait `T` expects `i64`");
    err("trait T {\n fn a(self) -> i64\n}\nstruct S {}\nimpl T for S {\n fn a() -> i64 { 1 }\n}", "should take `self`");
    err("trait T {\n fn a(self) -> i64\n}\nstruct S {}\nimpl T for S {\n fn a(self) -> i64 { 1 }\n fn extra(self) {}\n}", "not a member of trait `T`");
}

#[test]
fn impls_on_builtin_types() {
    ok("trait Double {\n fn double(self) -> Self\n}\nimpl Double for i64 {\n fn double(self) -> i64 { self * 2 }\n}\nfn main() { let x = 3.double() }");
}

#[test]
fn display_is_required_for_printing() {
    err("struct P {}\nfn main() { print(P()) }", "trait `Display` is not implemented for `P`");
    ok("struct P {}\nimpl Display for P {\n fn fmt(self) -> String { String.from(\"p\") }\n}\nfn main() { print(P()) }");
    err("struct P {}\nfn main() {\n let p = P()\n print(\"value: {p}\")\n}", "trait `Display` is not implemented");
}

// ------------------------------------------------------------------ closures, ranges, loops

#[test]
fn closures_infer_from_their_context() {
    assert_eq!(m("let f = |x: f64| x * 2.0", "f"), "fn(f64) -> f64");
    assert_eq!(m("let v = [1, 2, 3].map(|x| x * 2)", "v"), "Vec<i64>");
    assert_eq!(m("let v = [1.5, 2.5].filter(|x| x > 2.0)", "v"), "Vec<f64>");
    assert_eq!(m("let s = [1, 2, 3].sum()", "s"), "i64");
    ok("fn apply(f: fn(f64) -> f64, x: f64) -> f64 { f(x) }\nfn main() { apply(|x| x + 1.0, 2.0) }");
    err("fn apply(f: fn(f64) -> f64, x: f64) -> f64 { f(x) }\nfn main() { apply(|x| \"s\", 2.0) }", "mismatched types");
}

#[test]
fn functions_are_values() {
    ok("fn dbl(x: i64) -> i64 { x * 2 }\nfn main() { let f = dbl\n let y = f(3) }");
    ok("fn apply(f: fn(f64) -> f64, x: f64) -> f64 { f(x) }\nfn main() { apply(sin, 1.0) }");
}

#[test]
fn loops() {
    assert_eq!(m("let mut n = 0\nwhile n < 10 { n += 1 }", "n"), "i64");
    em("while 1 { }", "mismatched types");
    assert_eq!(m("let x = loop { break 5 }", "x"), "i64");
    em("break", "outside of a loop");
    em("continue", "outside of a loop");
    em("for x in 5 { }", "not iterable");
    em("for x in 0.0..1.0 { }", "range of floats");
}

#[test]
fn for_loops_bind_element_types() {
    assert_eq!(m("let v = vec[1.0, 2.0]\nfor x in v { }", "x"), "f64");
    assert_eq!(m("for (a, b) in vec[(1, \"x\")] { }", "b"), "str");
    assert_eq!(m("for i in 0..3 { }", "i"), "i64");
}

#[test]
fn if_expressions() {
    assert_eq!(m("let x = if true { 1 } else { 2 }", "x"), "i64");
    em("let x = if true { 1 } else { \"s\" }", "mismatched types");
    em("let x = if 1 { 1 } else { 2 }", "mismatched types");
}

#[test]
fn diverging_branches_do_not_constrain_the_type() {
    ok("fn f(n: i64) -> i64 { if n > 0 { n } else { return 0 } }");
    ok("fn f(n: i64) -> i64 { if n > 0 { n } else { panic(\"negative\") } }");
}

// ------------------------------------------------------------------ errors and `?`

#[test]
fn question_mark() {
    ok("fn f(a: Result<i64, str>) -> Result<i64, str> {\n let x = a?\n Ok(x + 1)\n}");
    err("fn f(a: Result<i64, str>) -> Result<i64, i64> {\n let x = a?\n Ok(x)\n}", "cannot convert the error `str` into `i64`");
    err("fn f(a: Result<i64, str>) -> i64 {\n let x = a?\n x\n}", "only be used in a function that returns `Result`");
    err("fn f(a: i64) -> Result<i64, str> {\n let x = a?\n Ok(x)\n}", "can only be applied to `Result` or `Option`");
    ok("fn f(a: Option<i64>) -> Option<i64> {\n let x = a?\n Some(x)\n}");
}

#[test]
fn std_modules_and_imports() {
    ok("use std.fs\nfn main() {\n let t = fs.read(\"x\")\n}");
    ok("use std.map.Map\nfn main() {\n let m = Map<str, i64>.new()\n}");
    ok("use math.fft.{fft, ifft}\nfn f(x: Tensor<f64, [_]>) { let y = fft(x) }");
    err("use nothing.here\nfn main() {}", "unresolved import `nothing.here`");
    err("use std.nope\nfn main() {}", "unresolved import `std.nope`");
    err("fn main() { let t = fs.read(\"x\") }", "cannot find value `fs`");
}

// ------------------------------------------------------------------ mutability and modes

#[test]
fn assignment_requires_mut() {
    em("let x = 1\nx = 2", "not declared `mut`");
    ok("fn main() {\n let mut x = 1\n x = 2\n}");
    em("let x = 1\nx += 2", "not declared `mut`");
    err("fn f(v: [f64]) { v[0] = 1.0 }", "not declared `mut`");
    ok("fn f(v: mut [f64]) { v[0] = 1.0 }");
    err("const K: i64 = 1\nfn main() { K = 2 }", "cannot assign to the constant");
    err("struct P { x: i64 }\nfn main() {\n let p = P(1)\n p.x = 2\n}", "not declared `mut`");
    err("fn f(x: i64) { x = 2 }", "not declared `mut`");
}

#[test]
fn call_site_mut_markers() {
    let f = "fn scale(v: mut [f64], k: f64) {\n for x in mut v { x *= k }\n}\nfn plain(v: [f64]) {}\n";
    ok(&format!("{f}fn main() {{\n let mut d = vec[1.0]\n scale(mut d, 2.0)\n}}"));
    err(&format!("{f}fn main() {{\n let mut d = vec[1.0]\n scale(d, 2.0)\n}}"), "must be marked `mut`");
    err(&format!("{f}fn main() {{\n let mut d = vec[1.0]\n plain(mut d)\n}}"), "unexpected `mut`");
    err(&format!("{f}fn main() {{\n let d = vec[1.0]\n scale(mut d, 2.0)\n}}"), "not declared `mut`");
    em("let mut x = 1\nlet y = mut x", "`mut` is only allowed on call arguments");
}

#[test]
fn for_over_mut_binds_mutable_elements() {
    ok("fn f(v: mut [f64]) {\n for x in mut v { x += 1.0 }\n}");
    err("fn f(v: mut [f64]) {\n for x in v { x += 1.0 }\n}", "not declared `mut`");
    err("fn f(v: [f64]) {\n for x in mut v { x += 1.0 }\n}", "not declared `mut`");
}

#[test]
fn own_parameters_are_mutable_inside() {
    ok("fn f(own v: Vec<i64>) { v.push(1) }");
    err("fn f(v: Vec<i64>) { v.push(1) }", "not declared `mut`");
}

// ------------------------------------------------------------------ indexing and collections

#[test]
fn indexing() {
    assert_eq!(m("let a = [1.0, 2.0]\nlet x = a[0]", "x"), "f64");
    assert_eq!(m("let v = vec[1, 2, 3]\nlet s = v[0..2]", "s"), "[i64]");
    em("let a = [1, 2]\nlet x = a[\"k\"]", "mismatched types");
    em("let a = [1, 2]\nlet x = a[0, 1]", "exactly one index");
    em("let x = 5\nlet y = x[0]", "cannot index");
}

#[test]
fn string_types() {
    assert_eq!(m("let s = \"hi\"\nlet t = s.to_string()", "t"), "String");
    assert_eq!(m("let s = String.from(\"a\")\nlet n = s.len", "n"), "usize");
    // String coerces to str where a str is expected
    ok("fn f(s: str) {}\nfn main() {\n let t = String.from(\"a\")\n f(t)\n}");
    err("fn f(s: String) {}\nfn main() { f(\"a\") }", "mismatched types");
}

#[test]
fn arrays_and_vecs_coerce_to_slices() {
    ok("fn f(v: [f64]) {}\nfn main() {\n f([1.0, 2.0])\n f(vec[1.0])\n}");
    // an integer literal may become a float, but a typed integer array may not
    ok("fn f(v: [f64]) {}\nfn main() { f([1, 2]) }");
    err("fn f(v: [f64]) {}\nfn main() { f([1u8, 2u8]) }", "mismatched types");
}

// ------------------------------------------------------------------ tensors

#[test]
fn tensor_operations() {
    let pre = "fn main() {\n let a = Tensor<f64, [2, 3]>.zeros()\n let b = Tensor<f64, [3, 4]>.zeros()\n";
    assert_eq!(ty(&format!("{pre} let c = a @ b\n}}"), "c"), "Tensor<f64, [2, 4]>");
    assert_eq!(ty(&format!("{pre} let c = a * 2.0\n}}"), "c"), "Tensor<f64, [2, 3]>");
    assert_eq!(ty(&format!("{pre} let c = 2.0 * a\n}}"), "c"), "Tensor<f64, [2, 3]>");
    assert_eq!(ty(&format!("{pre} let v = Tensor<f64, [3]>.ones()\n let c = a @ v\n}}"), "c"), "Tensor<f64, [2]>");
    assert_eq!(ty(&format!("{pre} let x = a[0, 1]\n}}"), "x"), "f64");
    err(&format!("{pre} let c = a @ a\n}}"), "shapes do not line up");
    err(&format!("{pre} let c = a + b\n}}"), "mismatched types");
    err(&format!("{pre} let x = a[0]\n}}"), "2 dimension(s), but 1 index");
}

#[test]
fn tensor_constructors_check_their_arguments() {
    err("fn main() { let a = Tensor<f64, [_, _]>.zeros() }", "size argument");
    err("fn main() { let a = Tensor<f64>.zeros() }", "takes an element type and a shape");
    ok("fn main() { let a = Tensor<f64, [_, _]>.random(3, 4) }");
}

// ------------------------------------------------------------------ parallel constructs

#[test]
fn parallel_reductions() {
    assert_eq!(m("let s = par sum(i in 0..10) i * i", "s"), "i64");
    assert_eq!(m("let s = par sum(i in 0..10) 0.5 * i as f64", "s"), "f64");
    assert_eq!(m("let b = par all(i in 0..10) i < 5", "b"), "bool");
    em("let s = par sum(i in 0..3) \"x\"", "trait `Num` is not implemented for `str`");
    em("let s = par frobnicate(i in 0..3) i", "unknown reduction");
}

#[test]
fn spawn_and_join() {
    assert_eq!(m("let h = spawn { 1 + 2 }", "h"), "Task<i64>");
    assert_eq!(m("let h = spawn { 1 + 2 }\nlet r = h.join()", "r"), "Result<i64, TaskPanicked>");
}

#[test]
fn par_for_loops() {
    ok("fn f(a: [f64], out: mut [f64]) {\n par for i in 0..a.len {\n out[i] = a[i]\n }\n}");
    err("fn f(a: [f64], out: [f64]) {\n par for i in 0..a.len {\n out[i] = a[i]\n }\n}", "not declared `mut`");
}

// ------------------------------------------------------------------ names and scoping

#[test]
fn undefined_names() {
    em("let x = nope", "cannot find value `nope`");
    em("nope(1)", "cannot find value `nope`");
    err("fn f(x: Nope) {}", "cannot find type `Nope`");
    err("struct S { a: Nope }", "cannot find type `Nope`");
}

#[test]
fn duplicate_definitions() {
    err("fn f() {}\nfn f() {}", "defined multiple times");
    err("struct S {}\nstruct S {}", "defined multiple times");
    err("struct S { a: i64, a: i64 }", "declared twice");
    err("enum A {\n X\n}\nenum B {\n X\n}", "defined multiple times");
}

#[test]
fn shadowing_and_scopes() {
    assert_eq!(m("let x = 1\nlet x = \"s\"", "x"), "i64"); // the first binding
    let r = ok("fn main() {\n let x = 1\n {\n let x = \"s\"\n }\n let y = x\n}");
    assert_eq!(r.binding_ty("y").unwrap(), "i64");
    em("{\n let inner = 1\n}\nlet y = inner", "cannot find value `inner`");
}

#[test]
fn user_definitions_shadow_the_prelude() {
    ok("fn sum(a: i64, b: i64) -> i64 { a + b }\nfn main() { let x = sum(1, 2) }");
}

#[test]
fn type_aliases() {
    assert_eq!(ty("type Grid = Tensor<f64, [_, _]>\nfn main() {\n let g = Grid.random(2, 2)\n}", "g"), "Tensor<f64, [_, _]>");
    err("type A = B\ntype B = A", "refers to itself");
}

#[test]
fn unsafe_functions_need_unsafe_blocks() {
    err("fn main() { let p = alloc<u8>(4) }", "requires an `unsafe` block");
    ok("fn main() { unsafe { let p = alloc<u8>(4)\n p[0] = 1u8 } }");
    ok("unsafe fn danger() -> i64 { 1 }\nfn main() { unsafe { danger() } }");
}

#[test]
fn self_only_in_methods() {
    err("fn f(self) {}", "`self` is only allowed in methods");
    err("fn f() -> Self { }", "`Self` can only be used");
}

#[test]
fn functions_need_bodies() {
    err("fn f() -> i64", "has no body");
}

#[test]
fn unknown_types_and_arity() {
    err("fn f(x: Vec) {}", "expects 1 type argument");
    err("fn f(x: Option<i64, i64>) {}", "expects 1 type argument");
    err("fn f(x: i64<i64>) {}", "does not take type arguments");
    err("fn f(x: Add) {}", "is a trait, not a type");
}

// ------------------------------------------------------------------ no cascades

#[test]
fn one_mistake_gives_one_error() {
    assert_eq!(count("fn main() { let x = nope + 1 }"), 1);
    assert_eq!(count("fn main() { print(nope) }"), 1);
    assert_eq!(count("fn main() { let v = vec[1, 2]\n let a = v.missing() }"), 1);
    assert_eq!(count("struct S { x: i64 }\nfn main() { let s = S(1)\n print(s.y) }"), 1);
}

#[test]
fn types_are_never_silently_errors_in_accepted_programs() {
    // if a program checks cleanly, no recorded type may be the error type
    for src in [
        "fn main() { let v = vec[1, 2, 3]\n let s = v.sum()\n print(s) }",
        "fn main() { let t = (1, 2.0)\n let (a, b) = t\n print(a) }",
    ] {
        let r = ok(src);
        assert!(r.results.bindings.iter().all(|b| !format!("{:?}", b.ty).contains("Error")), "{src}");
    }
}

// ------------------------------------------------------------------ infinite-size types

#[test]
fn a_type_that_contains_itself_is_rejected() {
    err("enum List {\n Nil\n Cons(i64, List)\n}", "contains itself");
    err("struct Node { next: Node }", "contains itself");
    err("struct Node { next: Option<Node> }", "contains itself");
    err("struct Node { pair: (i64, Node) }", "contains itself");
    err("struct Node { arr: [Node; 2] }", "contains itself");
}

#[test]
fn mutually_recursive_types_are_rejected() {
    let r = err("struct A { b: B }\nstruct B { a: A }", "contains itself");
    assert_eq!(r.diagnostics.len(), 2, "each type is reported");
}

#[test]
fn a_type_may_refer_to_itself_through_a_vec() {
    ok("struct Tree {\n value: i64\n children: Vec<Tree>\n}");
    ok("enum Json {\n Num(f64)\n List(Vec<Json>)\n}");
    ok("struct Graph {\n edges: Vec<(usize, usize)>\n names: Vec<String>\n}");
}

#[test]
fn recursion_through_generic_wrappers_is_found() {
    err("struct W<T> { inner: T }\nstruct Bad { w: W<Bad> }", "contains itself");
    ok("struct W<T> { inner: T }\nstruct Fine { w: W<i64> }");
}

// ------------------------------------------------------------------ Box<T>

mod boxes {
    use super::{err, ok};

    #[test]
    fn a_box_holds_a_value_and_gives_it_back() {
        let r = ok("fn main() {\n let b = Box.new(5)\n let n = b.take()\n let f = Box.new(2.5)\n}\n");
        assert_eq!(r.binding_ty("n").as_deref(), Some("i64"));
        assert_eq!(r.binding_ty("b").as_deref(), Some("Box<i64>"));
    }

    #[test]
    fn a_box_converts_only_where_a_borrow_is_wanted() {
        // a borrowed parameter takes the box as the value it holds
        ok("fn f(x: i64) -> i64 { x }\nfn main() {\n let b = Box.new(5)\n print(f(b))\n}\n");
        ok("fn f(x: Box<i64>) -> i64 { 1 }\nfn main() {\n let b = Box.new(5)\n print(f(b))\n}\n");
        // an owning parameter does not
        err("fn f(own x: i64) -> i64 { x }\nfn main() {\n let b = Box.new(5)\n print(f(b))\n}\n", "mismatched types");
        // nor does a `let` with a type
        err("fn main() {\n let b = Box.new(5)\n let x: i64 = b\n}\n", "mismatched types");
    }

    #[test]
    fn recursive_types_need_a_box_or_a_vector() {
        err("enum L {\n Nil\n Cons(i64, L)\n}\nfn main() {}\n", "contains itself");
        ok("enum L {\n Nil\n Cons(i64, Box<L>)\n}\nfn main() {}\n");
        ok("struct T { kids: Vec<T> }\nfn main() {}\n");
    }

    #[test]
    fn boxes_compare_and_print_like_what_they_hold() {
        ok("fn main() {\n let a = Box.new(1)\n let b = Box.new(2)\n print(a)\n assert(a < b)\n assert_eq(a, a)\n}\n");
        err("struct P { x: i64 }\nfn main() {\n let a = Box.new(P(1))\n print(a)\n}\n", "Display");
    }

    #[test]
    fn a_missing_field_or_method_through_a_box_is_reported() {
        err("struct P { x: i64 }\nfn main() {\n let b = Box.new(P(1))\n print(b.y)\n}\n", "no field `y`");
        err("struct P { x: i64 }\nfn main() {\n let b = Box.new(P(1))\n b.nothing()\n}\n", "no method named `nothing`");
    }
}

// ------------------------------------------------------------------ format specs

mod format_specs {
    fn errors(src: &str) -> String {
        let r = tau::analyze_source(src).unwrap_or_else(|d| panic!("{}", d.render(src, "t.tau")));
        r.diagnostics.iter().map(|d| d.render(src, "t.tau")).collect::<Vec<_>>().join("\n")
    }

    fn main_with(body: &str) -> String {
        format!("fn main() {{\n{body}\n}}")
    }

    #[test]
    fn specs_must_fit_the_value() {
        let bad = [
            ("let n = 5\n print(\"{n:.2}\")", "a precision (`.N`) cannot be used on an integer"),
            ("let n = 5\n print(\"{n:e}\")", "`e` (scientific notation) cannot be used on an integer"),
            ("let x = 2.5\n print(\"{x:x}\")", "cannot be used on a float"),
            ("let x = 2.5\n print(\"{x:#}\")", "`#` cannot be used on a float"),
            ("let s = \"a\"\n print(\"{s:08}\")", "cannot be used on text"),
            ("let s = \"a\"\n print(\"{s:+}\")", "cannot be used on text"),
            ("let c = 'a'\n print(\"{c:.2}\")", "cannot be used on a character or bool"),
            ("let v = vec[1]\n print(\"{v:x}\")", "cannot be used on this type"),
            ("let z = 1.0 + 2.0i\n print(\"{z:+}\")", "cannot be used on a complex number"),
        ];
        for (body, want) in bad {
            let e = errors(&main_with(body));
            assert!(e.contains("this format does not fit the value"), "{body}: {e}");
            assert!(e.contains(want), "{body}: expected {want:?} in {e}");
        }
    }

    #[test]
    fn good_specs_are_accepted() {
        let good = "let n = 5\n let x = 2.5\n let s = \"a\"\n let z = 1.0 + 2.0i\n let v = vec[1]\n let t = (1, 2)\n let c = 'a'\n let b = true\n print(\"{n:08x} {n:+} {n:#b} {x:+010.3e} {x:.0} {s:>6.2} {z:.1} {v:>12} {t:<9} {c:^3} {b:5}\")";
        assert_eq!(errors(&main_with(good)), "");
    }

    #[test]
    fn a_literal_that_is_not_yet_typed_is_checked_after_defaulting() {
        // `5` becomes an i64, so a precision is wrong; `2.5` becomes an f64, so `x` is wrong
        assert!(errors(&main_with("print(\"{5:.2}\")")).contains("an integer"));
        assert!(errors(&main_with("print(\"{2.5:x}\")")).contains("a float"));
        assert_eq!(errors(&main_with("print(\"{5:x} {2.5:.2}\")")), "");
    }
}

// ------------------------------------------------------------------ tensor constructors and methods

mod tensor_api {
    use super::{err, ok};

    #[test]
    fn the_new_constructors_and_methods_have_the_right_types() {
        let r = ok("fn main() {\n let a = Tensor<f64, [_]>.from([1.0, 2.0])\n let m = Tensor<f64, [_, _]>.eye(2)\n let s = a.sum()\n let sh = a.shape()\n let v = a.to_vec()\n let r = m.reshape(4)\n let t = m.transpose()\n let b = a.map(|x| x + 1.0)\n let c = Tensor<c128, [_]>.from([1.0 + 0.0i])\n let z = c.sum()\n}\n");
        assert_eq!(r.binding_ty("s").as_deref(), Some("f64"));
        assert_eq!(r.binding_ty("sh").as_deref(), Some("Vec<usize>"));
        assert_eq!(r.binding_ty("v").as_deref(), Some("Vec<f64>"));
        assert_eq!(r.binding_ty("r").as_deref(), Some("Tensor<f64, [_]>"));
        assert_eq!(r.binding_ty("t").as_deref(), Some("Tensor<f64, [_, _]>"));
        assert_eq!(r.binding_ty("b").as_deref(), Some("Tensor<f64, [_]>"));
        assert_eq!(r.binding_ty("z").as_deref(), Some("c128"));
    }

    #[test]
    fn misuse_is_explained() {
        err("fn main() {\n let a = Tensor<f64, [_, _]>.from([1.0])\n}\n", "makes a tensor of rank 1");
        err("fn main() {\n let a = Tensor<f64, [_]>.eye(3)\n}\n", "makes a tensor of rank 2");
        err("fn main() {\n let a = Tensor<f64, [_]>.arange(2, 3)\n}\n", "takes 1 argument");
        err("fn main() {\n let a = Tensor<c128, [_]>.linspace(0.0, 1.0, 3)\n}\n", "needs a tensor of floats");
        err("fn main() {\n let a = Tensor<c128, [_]>.arange(3)\n let b = a.min()\n}\n", "needs a tensor of floats");
        err("fn main() {\n let a = Tensor<f64, [_]>.arange(3)\n let b = a.transpose()\n}\n", "rank 2");
        err("fn main() {\n let a = Tensor<f64, [_]>.arange(3)\n let b = a.reshape()\n}\n", "needs the new sizes");
        err("fn main() {\n let a = Tensor<f64, [_]>.arange(3)\n let b = a.map(|x: i64| x)\n}\n", "mismatched types");
        err("fn main() {\n let a = Tensor<f64, [_]>.arange(3)\n let b = a.sum(1)\n}\n", "takes 0 arguments");
    }
}

mod tensor_slices {
    use super::{err, ok};

    #[test]
    fn a_range_keeps_its_axis() {
        let r = ok("fn main() {\n let m = Tensor<f64, [_, _]>.zeros(3, 3)\n let a = m[1, 0]\n let b = m[1, ..]\n let c = m[0..2, 1..]\n let d = m[.., 2]\n let e = m[..=1, ..=1]\n}\n");
        assert_eq!(r.binding_ty("a").as_deref(), Some("f64"));
        assert_eq!(r.binding_ty("b").as_deref(), Some("Tensor<f64, [_]>"));
        assert_eq!(r.binding_ty("c").as_deref(), Some("Tensor<f64, [_, _]>"));
        assert_eq!(r.binding_ty("d").as_deref(), Some("Tensor<f64, [_]>"));
        assert_eq!(r.binding_ty("e").as_deref(), Some("Tensor<f64, [_, _]>"));
    }

    #[test]
    fn parts_can_be_assigned_a_tensor_or_one_number() {
        ok("fn main() {\n let mut m = Tensor<f64, [_, _]>.zeros(3, 3)\n m[0, ..] = 1.0\n m[.., 1] = Tensor<f64, [_]>.arange(3)\n m[0..2, 0..2] = m[1..3, 1..3]\n}\n");
        err("fn main() {\n let m = Tensor<f64, [_, _]>.zeros(3, 3)\n m[0, ..] = 1.0\n}\n", "not declared `mut`");
        err("fn main() {\n let mut m = Tensor<f64, [_, _]>.zeros(3, 3)\n m[0, ..] += 1.0\n}\n", "cannot be combined with assignment");
        err("fn main() {\n let mut m = Tensor<f64, [_, _]>.zeros(3, 3)\n m[0, ..] = \"x\"\n}\n", "mismatched types");
        err("fn main() {\n let mut m = Tensor<f64, [_, _]>.zeros(3, 3)\n m[0, ..] = Tensor<f32, [_]>.zeros(3)\n}\n", "mismatched types");
    }

    #[test]
    fn the_number_of_indices_and_their_types_are_checked() {
        err("fn main() {\n let m = Tensor<f64, [_, _]>.zeros(3, 3)\n let a = m[1..2]\n}\n", "2 dimension(s), but 1 index");
        err("fn main() {\n let m = Tensor<f64, [_, _]>.zeros(3, 3)\n let a = m[\"a\"..2, 0]\n}\n", "mismatched types");
        err("fn main() {\n let m = Tensor<f64, [_, _]>.zeros(3, 3)\n let a = m[1.5, ..]\n}\n", "mismatched types");
    }
}

mod iterators {
    use super::*;

    const COUNTER: &str = "struct Counter { n: i64, end: i64 }\nimpl Iterator<i64> for Counter {\n fn next(mut self) -> Option<i64> {\n  if self.n < self.end {\n   self.n += 1\n   Some(self.n)\n  } else { None }\n }\n}\n";

    #[test]
    fn chains_have_the_types_of_their_items() {
        assert_eq!(m("let v: Vec<f64> = vec[1.0, 2.0]\n let s = v.iter().map(|x| x * 2.0).sum()", "s"), "f64");
        assert_eq!(m("let v = vec[1, 2]\n let c = v.iter().enumerate().collect()", "c"), "Vec<(usize, i64)>");
        assert_eq!(m("let v = vec[1, 2]\n let c = v.iter().zip((0..2).iter()).count()", "c"), "usize");
        assert_eq!(m("let v: Vec<i32> = vec[1, 2]\n let c = v.iter().max()", "c"), "Option<i32>");
        assert_eq!(m("let v = vec[1, 2]\n let c = v.iter().map(|x| x > 1).all(|b| b)", "c"), "bool");
    }

    #[test]
    fn the_item_type_comes_from_the_impl() {
        // `total` learns `T` from `I: Iterator<T>`
        ok("fn total<T: Num, I: Iterator<T>>(own it: I) -> T {\n let mut s = it\n let mut a: T = zero()\n loop {\n  match s.next() {\n   Some(x) => a = a + x\n   None => break\n  }\n }\n a\n}\nfn main() {\n let c = Counter(0, 3)\n print(total(c))\n}\nstruct Counter { n: i64, end: i64 }\nimpl Iterator<i64> for Counter {\n fn next(mut self) -> Option<i64> { None }\n}\n");
    }

    #[test]
    fn a_type_with_next_can_be_looped_over() {
        ok(&format!("{COUNTER}fn main() {{\n for x in Counter(0, 3) {{ print(x + 1) }}\n let mut c = Counter(0, 3)\n for x in mut c {{ print(x) }}\n print(c.next())\n}}\n"));
        assert_eq!(ty(&format!("{COUNTER}fn main() {{\n for x in Counter(0, 3) {{ let y = x }}\n}}\n"), "y"), "i64");
    }

    #[test]
    fn only_an_iterator_impl_makes_a_type_iterable() {
        let r = err("struct S { n: i64 }\nimpl S { fn next(mut self) -> Option<i64> { None } }\nfn main() {\n for x in S(1) { print(x) }\n}\n", "is not iterable");
        assert!(r.diagnostics[0].hint.as_deref().unwrap_or("").contains("impl Iterator"), "{:?}", r.diagnostics[0].hint);
        err("fn main() {\n for x in 5 { print(x) }\n}\n", "is not iterable");
    }

    #[test]
    fn adapters_check_their_closures() {
        err("fn main() {\n let v = vec[1, 2]\n let n = v.iter().filter(|x| x + 1).count()\n}\n", "mismatched types");
        err("fn main() {\n let v = vec[1, 2]\n let n: i64 = v.iter().map(|x| x > 1).sum()\n}\n", "mismatched types");
        err("fn main() {\n let v = vec[\"a\"]\n print(v.iter().sum())\n}\n", "`Num` is not implemented for `str`");
        err("fn main() {\n let v = vec[1]\n let s = v.iter().step_by(1, 2)\n}\n", "argument");
    }

    #[test]
    fn option_and_result_have_display_when_their_contents_do() {
        ok("fn main() {\n print(Some(1))\n let r: Result<i64, String> = Ok(1)\n print(r)\n print(\"{Some(2.5)}\")\n}\n");
        err("struct P { x: i64 }\nfn main() {\n print(Some(P(1)))\n}\n", "`Display` is not implemented");
    }

    #[test]
    fn zero_one_and_clone_work_in_generic_code() {
        ok("fn f<T: Num>(x: T) -> T { let a: T = zero()\n let b: T = one()\n a + b + x }\nfn g<T: Clone>(x: T) -> T { x.clone() }\nfn main() { print(f(2)) \n print(g(vec[1])) }\n");
        err("fn f<T>() -> T { zero() }\nfn main() {}\n", "Num");
    }
}

mod shared_values {
    use super::*;

    #[test]
    fn shared_is_read_like_a_box_and_never_changed() {
        let r = ok("struct P { x: i64, v: Vec<i64> }\nfn len(p: P) -> usize { p.v.len }\nfn main() {\n let s = Shared.new(P(1, vec[1, 2]))\n let t = s.clone()\n let a = s.x\n let n = s.v.len\n let m = len(t)\n let i = s.v[0]\n}\n");
        assert_eq!(r.binding_ty("s").as_deref(), Some("Shared<P>"));
        assert_eq!(r.binding_ty("t").as_deref(), Some("Shared<P>"));
        assert_eq!(r.binding_ty("a").as_deref(), Some("i64"));
        assert_eq!(r.binding_ty("i").as_deref(), Some("i64"));
        let msg = "cannot change what is inside a `Shared`";
        err("fn main() {\n let s = Shared.new(vec[1])\n s.push(2)\n}\n", msg);
        err("struct P { x: i64 }\nfn main() {\n let mut s = Shared.new(P(1))\n s.x = 5\n}\n", msg);
        err("fn main() {\n let mut s = Shared.new(vec[1])\n s[0] = 5\n}\n", msg);
        err("fn grow(v: mut Vec<i64>) { v.push(1) }\nfn main() {\n let mut s = Shared.new(vec[1])\n grow(mut s)\n}\n", msg);
        // an error message with the way out
        let r = err("fn main() {\n let mut s = Shared.new(vec[1])\n s.push(2)\n}\n", msg);
        assert!(r.diagnostics[0].hint.as_deref().unwrap_or("").contains("Mutex"), "{:?}", r.diagnostics[0].hint);
    }

    #[test]
    fn a_handle_is_moved_and_clone_makes_another() {
        ok("fn main() {\n let s = Shared.new(vec[1])\n let t = s.clone()\n print(s.len + t.len)\n}\n");
    }

    #[test]
    fn mutex_methods_and_what_a_mutex_is_not() {
        let r = ok("fn main() {\n let m = Mutex.new(0)\n m.set(1)\n m.update(|x| x + 1)\n let a = m.get()\n let b = m.with(|x| x > 0)\n let c = m.into_inner()\n}\n");
        assert_eq!(r.binding_ty("m").as_deref(), Some("Mutex<i64>"));
        assert_eq!(r.binding_ty("a").as_deref(), Some("i64"));
        assert_eq!(r.binding_ty("b").as_deref(), Some("bool"));
        err("fn main() {\n let m = Mutex.new(1)\n let n = m.clone()\n}\n", "no method named `clone`");
        err("fn main() {\n let m = Mutex.new(1)\n print(m)\n}\n", "`Display` is not implemented for `Mutex<i64>`");
        err("fn main() {\n let m = Mutex.new(1)\n m.update(|x| x > 0)\n}\n", "mismatched types");
    }

    #[test]
    fn any_cloneable_value_can_be_cloned() {
        let r = ok("fn main() {\n let v = vec[1, 2]\n let w = v.clone()\n let s = String.from(\"a\").clone()\n let b = Box.new(3).clone()\n let t = (1, 2.0).clone()\n}\n");
        assert_eq!(r.binding_ty("w").as_deref(), Some("Vec<i64>"));
        assert_eq!(r.binding_ty("s").as_deref(), Some("String"));
        assert_eq!(r.binding_ty("t").as_deref(), Some("(i64, f64)"));
    }

    #[test]
    fn a_box_or_shared_is_matched_through() {
        ok("enum T {\n Leaf\n Node(Shared<T>, i64)\n}\nfn f(t: Shared<T>) -> i64 {\n match t {\n  Leaf => 0\n  Node(_, v) => v\n }\n}\nfn main() {}\n");
    }
}

mod integer_tensors {
    use super::*;

    #[test]
    fn what_a_tensor_may_hold() {
        ok("fn main() {\n let a = Tensor<i8, [_]>.zeros(2)\n let b = Tensor<u64, [_]>.zeros(2)\n let c = Tensor<bool, [_]>.zeros(2)\n let d = Tensor<usize, [_, _]>.eye(2)\n}\n");
        err("fn main() {\n let a: Tensor<i128, [_]> = Tensor<i128, [_]>.zeros(2)\n}\n", "cannot be the element type of a tensor");
        err("fn main() {\n let a: Tensor<char, [_]> = Tensor<char, [_]>.zeros(2)\n}\n", "cannot be the element type of a tensor");
        err("fn f(a: Tensor<String, [_]>) {}\nfn main() {}\n", "cannot be the element type of a tensor");
    }

    #[test]
    fn the_types_of_results() {
        let r = ok("fn main() {\n let a = Tensor<i64, [_]>.from([1, 2, 3])\n let m = a > 1\n let s = a.sum()\n let c = m.count()\n let f = a.to_f64()\n let i = f.to_i64()\n let v = a.select(m)\n let n = !m\n let k = a * 2\n}\n");
        assert_eq!(r.binding_ty("m").as_deref(), Some("Tensor<bool, [_]>"));
        assert_eq!(r.binding_ty("s").as_deref(), Some("i64"));
        assert_eq!(r.binding_ty("c").as_deref(), Some("usize"));
        assert_eq!(r.binding_ty("f").as_deref(), Some("Tensor<f64, [_]>"));
        assert_eq!(r.binding_ty("i").as_deref(), Some("Tensor<i64, [_]>"));
        assert_eq!(r.binding_ty("v").as_deref(), Some("Tensor<i64, [_]>"));
        assert_eq!(r.binding_ty("n").as_deref(), Some("Tensor<bool, [_]>"));
        assert_eq!(r.binding_ty("k").as_deref(), Some("Tensor<i64, [_]>"));
        let r = ok("fn main() {\n let f = Tensor<f64, [_, _]>.zeros(2, 2)\n let m = f >= 0.5\n let e = f == f\n let x = Tensor<u8, [_]>.arange(3)\n}\n");
        assert_eq!(r.binding_ty("m").as_deref(), Some("Tensor<bool, [_, _]>"));
        assert_eq!(r.binding_ty("e").as_deref(), Some("Tensor<bool, [_, _]>"));
    }

    #[test]
    fn methods_ask_for_the_right_elements() {
        err("fn main() {\n let a = Tensor<i64, [_]>.from([1, 2])\n print(a.mean())\n}\n", "`mean` needs a tensor of floats");
        err("fn main() {\n let a = Tensor<i64, [_]>.from([1, 2])\n print(a.count())\n}\n", "`count` needs a tensor of `bool`");
        err("fn main() {\n let a = Tensor<bool, [_]>.zeros(2)\n print(a.sum())\n}\n", "`sum` needs a tensor of numbers");
        err("fn main() {\n let a = Tensor<i64, [_]>.from([1, 2])\n print(a.select(a))\n}\n", "needs a mask");
        err("fn main() {\n let a = Tensor<i64, [_]>.random(2)\n}\n", "`random` needs a tensor of floats");
        err("fn main() {\n let a = Tensor<i64, [_]>.linspace(0, 1, 3)\n}\n", "needs a tensor of floats");
        err("fn main() {\n let a = Tensor<bool, [_]>.arange(3)\n}\n", "`arange` needs a tensor of floats or integers");
        let c = complex_source();
        err(&c, "cannot be converted to `f64`");
    }

    fn complex_source() -> String {
        "fn main() {\n let c = Tensor<c128, [_]>.zeros(2)\n print(c.to_f64())\n}\n".to_string()
    }

    #[test]
    fn operators_on_integer_and_bool_tensors() {
        ok("fn main() {\n let a = Tensor<i64, [_]>.from([1, 2])\n let b = a % 2 + a * a - a / 1\n let c = (a & 3) | (a ^^ 1)\n let d = Tensor<bool, [_]>.zeros(2)\n let e = d & !d | d ^^ d\n}\n");
        err("fn main() {\n let d = Tensor<bool, [_]>.zeros(2)\n let e = d + d\n}\n", "does not work on tensors of `bool`");
        err("fn main() {\n let d = Tensor<bool, [_]>.zeros(2)\n let e = -d\n}\n", "cannot negate a tensor of `bool`");
        err("fn main() {\n let d = Tensor<f64, [_]>.zeros(2)\n let e = d & d\n}\n", "only works on tensors of integers");
        err("fn main() {\n let d = Tensor<f64, [_]>.zeros(2)\n let e = d % d\n}\n", "only works on tensors of integers");
        err("fn main() {\n let d = Tensor<i64, [_]>.zeros(2)\n let e = !d\n}\n", "mismatched types");
        err("fn main() {\n let d = Tensor<bool, [_]>.zeros(2)\n let e = d < d\n}\n", "cannot compare tensors of `bool`");
        err("fn main() {\n let a = Tensor<i64, [_]>.zeros(2)\n let e = a == 1.5\n}\n", "mismatched types");
        err("fn main() {\n let a = Tensor<i64, [_]>.zeros(2)\n let b = Tensor<i32, [_]>.zeros(2)\n let e = a == b\n}\n", "mismatched types");
        // a comparison is a tensor, not a `bool`
        err("fn main() {\n let a = Tensor<i64, [_]>.zeros(2)\n if a == a { print(1) }\n}\n", "mismatched types");
    }
}
