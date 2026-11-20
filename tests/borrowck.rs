//! Borrow-checker tests: each rejected program must produce the expected error, and
//! each safe look-alike must be accepted.

use tau::analyze_source;
use tau::CheckResult;

fn run(src: &str) -> CheckResult {
    match analyze_source(src) {
        Ok(r) => r,
        Err(d) => panic!("parse error:\n{}", d.render(src, "<test>")),
    }
}

fn render(src: &str, r: &CheckResult) -> String {
    r.diagnostics.iter().map(|d| d.render(src, "<test>")).collect::<Vec<_>>().join("\n")
}

/// The program must pass type checking and borrow checking.
fn ok(src: &str) {
    let r = run(src);
    assert!(r.diagnostics.is_empty(), "unexpected errors:\n{}", render(src, &r));
}

/// The program must produce exactly one error, containing `needle`.
fn err(src: &str, needle: &str) {
    let r = run(src);
    let msgs: Vec<&str> = r.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert!(
        msgs.iter().any(|m| m.contains(needle)),
        "expected an error containing {needle:?}, got {msgs:?}\n{}",
        render(src, &r)
    );
    assert_eq!(msgs.len(), 1, "expected exactly one error, got {msgs:?}\n{}", render(src, &r));
}

const LIB: &str = "\
fn eat(own v: Vec<i64>) {}
fn look(v: [i64]) {}
fn grow(v: mut Vec<i64>) {}
fn two(a: mut Vec<i64>, b: [i64]) {}
fn both(a: mut Vec<i64>, b: mut Vec<i64>) {}
struct P { name: String, age: i64 }
fn eat_s(own s: String) {}
";

fn m(body: &str) -> String {
    format!("{LIB}fn main() {{\n{body}\n}}")
}

fn ok_m(body: &str) {
    ok(&m(body));
}

fn err_m(body: &str, needle: &str) {
    err(&m(body), needle);
}

// ------------------------------------------------------------------ moves

#[test]
fn use_after_move() {
    err_m("let a = vec[1]\neat(a)\nprint(a.len)", "use of moved value `a`");
    err_m("let a = vec[1]\nlet b = a\nprint(a.len)", "use of moved value `a`");
    err_m("let a = vec[1]\neat(a)\neat(a)", "use of moved value `a`");
}

#[test]
fn copy_types_are_never_moved() {
    ok_m("let a = 5\nlet b = a\nprint(a)");
    ok_m("let a = (1, 2.0)\nlet b = a\nprint(a)");
    ok_m("let a = \"text\"\nlet b = a\nprint(a)");
    ok_m("let a = [1, 2, 3]\nlet b = a\nprint(a[0])");
    ok_m("let o = Some(3)\nlet p = o\nlet q = o");
}

#[test]
fn types_with_a_copy_impl_are_copied() {
    ok("struct V { x: f64 }\nimpl Copy for V {}\nfn main() {\n let a = V(1.0)\n let b = a\n print(a.x)\n}");
    err("struct V { x: f64 }\nfn main() {\n let a = V(1.0)\n let b = a\n print(a.x)\n}", "use of moved value `a`");
}

#[test]
fn borrowing_does_not_move() {
    ok_m("let a = vec[1]\nlook(a)\nlook(a)\nprint(a.len)");
    ok_m("let mut a = vec[1]\ngrow(mut a)\ngrow(mut a)\nlook(a)");
}

#[test]
fn moved_on_one_branch_is_moved() {
    err_m("let a = vec[1]\nif true { eat(a) }\nprint(a.len)", "use of moved value `a`");
    ok_m("let a = vec[1]\nif true { eat(a) } else { look(a) }");
    ok_m("let a = vec[1]\nif true { eat(a) } else { eat(a) }");
}

#[test]
fn moving_inside_a_loop() {
    err_m("let a = vec[1]\nfor i in 0..3 {\n eat(a)\n}", "use of moved value `a`");
    ok_m("for i in 0..3 {\n let a = vec[1]\n eat(a)\n}");
    ok_m("let a = vec[1]\nfor i in 0..3 {\n look(a)\n}");
    // moving and then leaving the loop is fine
    ok_m("let a = vec[1]\nloop {\n eat(a)\n break\n}");
}

#[test]
fn reassigning_gives_a_value_back() {
    ok_m("let mut a = vec[1]\neat(a)\na = vec[2]\nprint(a.len)");
    err_m("let mut a = vec[1]\neat(a)\nprint(a.len)\na = vec[2]", "use of moved value `a`");
}

#[test]
fn partial_moves_are_tracked_per_field() {
    let p = "let p = P(String.from(\"x\"), 3)\n";
    ok_m(&format!("{p}let n = p.name\nprint(p.age)"));
    err_m(&format!("{p}let n = p.name\nprint(p.name)"), "use of moved value `p.name`");
    err_m(&format!("{p}let n = p.name\nlet q = p"), "use of partially moved value `p`");
    ok_m(&format!("{p}let n = p.name\nlet a = p.age"));
}

#[test]
fn cannot_move_out_of_borrowed_parameters() {
    err("struct P { name: String }\nfn f(p: P) -> String { p.name }", "cannot move `p.name`");
    err("fn f(v: Vec<i64>) -> Vec<i64> { v }", "cannot move `v`");
    ok("struct P { name: String }\nfn f(own p: P) -> String { p.name }");
    ok("fn f(own v: Vec<i64>) -> Vec<i64> { v }");
    err("fn f(v: mut Vec<i64>) -> Vec<i64> { v }", "cannot move `v`");
    // copy fields can be read out of a borrowed parameter
    ok("struct P { name: String, age: i64 }\nfn f(p: P) -> i64 { p.age }");
}

#[test]
fn cannot_move_out_of_a_collection() {
    err("fn f(v: Vec<String>) {\n let s = v[0]\n}", "out of a collection");
    ok("fn f(v: Vec<i64>) {\n let s = v[0]\n}");
}

#[test]
fn view_bindings_cannot_be_moved() {
    err("fn f(names: Vec<String>) {\n for s in names {\n let t = s\n }\n}", "cannot move `s`");
    ok("fn f(names: Vec<String>) {\n for s in names {\n print(s)\n }\n}");
    err("fn f(o: Option<String>) {\n match o {\n Some(s) => eat_s(s)\n None => print(0)\n }\n}\nfn eat_s(own s: String) {}", "cannot move `s`");
    // a temporary is owned by the match, so its payload can be moved
    ok("fn mk() -> Option<String> { None }\nfn eat_s(own s: String) {}\nfn f() {\n match mk() {\n Some(s) => eat_s(s)\n None => print(0)\n }\n}");
}

#[test]
fn closures_that_move_a_capture_move_it() {
    err_m("let a = vec[1]\nlet f = || eat(a)\nprint(a.len)", "use of moved value `a`");
}

#[test]
fn uninitialised_variables() {
    err_m("let x: i64\nprint(x)", "possibly-uninitialized `x`");
    ok_m("let mut x: i64\nx = 5\nprint(x)");
    err_m("let mut x: i64\nif true { x = 1 }\nprint(x)", "possibly-uninitialized `x`");
    ok_m("let mut x: i64\nif true { x = 1 } else { x = 2 }\nprint(x)");
}

// ------------------------------------------------------------------ call arguments

#[test]
fn mut_argument_conflicts_within_a_call() {
    err_m("let mut a = vec[1]\ntwo(mut a, a)", "also used by another argument");
    err_m("let mut a = vec[1]\nboth(mut a, mut a)", "also used by another argument");
    ok_m("let mut a = vec[1]\nlet mut b = vec[2]\nboth(mut a, mut b)");
    ok_m("let mut a = vec[1]\nlet b = vec[2]\ntwo(mut a, b)");
}

#[test]
fn copying_a_value_before_the_call_is_fine() {
    ok_m("let mut v = vec[1, 2]\nv.push(v[0])");
    ok_m("let mut v = vec[1, 2]\nv.push(v.len as i64)");
}

#[test]
fn disjoint_fields_do_not_conflict() {
    ok("struct S { a: Vec<i64>, b: Vec<i64> }\nfn both(a: mut Vec<i64>, b: mut Vec<i64>) {}\nfn main() {\n let mut s = S(vec[1], vec[2])\n both(mut s.a, mut s.b)\n}");
    err("struct S { a: Vec<i64>, b: Vec<i64> }\nfn both(a: mut Vec<i64>, b: mut Vec<i64>) {}\nfn main() {\n let mut s = S(vec[1], vec[2])\n both(mut s.a, mut s.a)\n}", "also used by another argument");
}

// ------------------------------------------------------------------ views and loans

#[test]
fn a_view_keeps_its_source_borrowed() {
    err_m("let mut v = vec[1, 2, 3]\nlet s = v[0..2]\nv.push(4)\nprint(s.len)", "cannot borrow `v` as mutable");
    err_m("let mut v = vec[1, 2, 3]\nlet s = v[0..2]\nv = vec[9]\nprint(s.len)", "cannot assign to `v` because it is borrowed");
}

#[test]
fn borrows_end_at_their_last_use() {
    // non-lexical: the view is not used after the push
    ok_m("let mut v = vec[1, 2, 3]\nlet s = v[0..2]\nprint(s.len)\nv.push(4)");
    ok_m("let mut v = vec[1, 2, 3]\nlet n = v[0..2].len\nv.push(4)");
}

#[test]
fn shared_views_can_coexist() {
    ok_m("let v = vec[1, 2, 3]\nlet a = v[0..1]\nlet b = v[1..3]\nprint(a.len)\nprint(b.len)\nlook(v)");
}

#[test]
fn views_flow_through_copies_and_calls() {
    err_m("let mut v = vec[1, 2, 3]\nlet s = v[0..2]\nlet t = s\nv.push(4)\nprint(t.len)", "cannot borrow `v` as mutable");
    err("fn id(x: [i64]) -> [i64] { x }\nfn main() {\n let mut v = vec[1]\n let s = id(v)\n v.push(2)\n print(s.len)\n}", "cannot borrow `v` as mutable");
}

#[test]
fn modifying_a_collection_while_iterating_it() {
    err_m("let mut v = vec[1, 2]\nfor x in v {\n v.push(x)\n}", "cannot borrow `v` as mutable");
    ok_m("let mut v = vec[1, 2]\nfor x in mut v {\n x += 1\n}");
    err_m("let mut v = vec[1, 2]\nfor x in mut v {\n print(v.len)\n}", "cannot use `v` because it is mutably borrowed");
    ok_m("let mut v = vec[1, 2]\nfor i in 0..v.len {\n v.push(1)\n}");
}

#[test]
fn reading_while_a_mutable_borrow_is_alive() {
    err("fn f(a: mut Vec<i64>) -> Vec<i64> { a }\nfn main() {\n let mut v = vec[1]\n let w = mut v\n}", "`mut` is only allowed");
}

// ------------------------------------------------------------------ returning borrows

#[test]
fn returning_a_borrow_of_a_local() {
    err("fn f() -> [i64] {\n let v = vec[1, 2]\n v[0..1]\n}", "cannot return a value that borrows `v`");
    ok("fn f(v: [i64]) -> [i64] { v[0..1] }");
    ok("fn f(v: [i64]) -> [i64] { v }");
    ok("fn f(v: Vec<i64>) -> [i64] { v[0..1] }");
}

#[test]
fn returning_owned_values_is_always_fine() {
    ok("fn f() -> Vec<i64> {\n let v = vec[1, 2]\n v\n}");
    ok("fn f(v: [i64]) -> i64 { v[0] }");
}

// ------------------------------------------------------------------ closures

#[test]
fn closures_borrow_what_they_capture() {
    err_m("let mut v = vec[1]\nlet f = || v.len\nv.push(2)\nlet n = f()", "cannot borrow `v` as mutable");
    ok_m("let mut v = vec[1]\nlet f = || v.len\nlet n = f()\nv.push(2)");
}

#[test]
fn closures_that_mutate_hold_a_mutable_borrow() {
    err_m("let mut n = 0\nlet inc = || n += 1\nn = 5\ninc()", "cannot assign to `n` because it is borrowed");
    ok_m("let mut n = 0\nlet inc = || n += 1\ninc()\nn = 5");
}

#[test]
fn closures_passed_straight_to_calls() {
    ok_m("let v = vec[1, 2, 3]\nlet w = v.map(|x| x * 2)\nprint(w.len)");
    ok_m("let v = vec[1, 2, 3]\nlet k = 10\nlet w = v.map(|x| x + k)\nprint(k)");
}

// ------------------------------------------------------------------ tasks

#[test]
fn spawn_cannot_modify_captured_variables() {
    err_m("let mut n = 0\nlet h = spawn { n += 1 }\nh.join()", "cannot modify `n` from inside `spawn`");
}

#[test]
fn spawn_borrows_until_joined() {
    err_m("let mut v = vec[1]\nlet h = spawn { v.len }\nv.push(2)\nh.join()", "cannot borrow `v` as mutable");
    ok_m("let mut v = vec[1]\nlet h = spawn { v.len }\nh.join()\nv.push(2)");
    // an unjoined handle is joined when it goes out of scope, so it still holds the loan
    err_m("let mut v = vec[1]\nlet h = spawn { v.len }\nv.push(2)", "cannot borrow `v` as mutable");
}

#[test]
fn spawn_moves_what_it_consumes() {
    err_m("let v = vec[1]\nlet h = spawn { eat(v) }\nprint(v.len)", "use of moved value `v`");
    ok_m("let v = vec[1]\nlet h = spawn { eat(v) }\nh.join()");
}

#[test]
fn many_tasks_can_read_the_same_data() {
    ok_m("let v = vec[1, 2]\nlet a = spawn { v.len }\nlet b = spawn { v.len }\na.join()\nb.join()");
}

#[test]
fn a_joined_handle_cannot_be_joined_twice() {
    err_m("let h = spawn { 1 }\nh.join()\nh.join()", "use of moved value `h`");
}

// ------------------------------------------------------------------ parallel loops

const PAR: &str = "fn f(out: mut [f64], a: [f64]) {\n";

#[test]
fn par_for_may_write_its_own_index() {
    ok(&format!("{PAR} par for i in 0..a.len {{\n out[i] = a[i] * 2.0\n }}\n}}"));
    ok(&format!("{PAR} par for i in 0..a.len {{\n out[i] += a[i]\n }}\n}}"));
}

#[test]
fn par_for_rejects_other_writes() {
    err(&format!("{PAR} par for i in 0..a.len {{\n out[0] = a[i]\n }}\n}}"), "cannot modify `out[..]` inside a `par for`");
    err(&format!("{PAR} par for i in 0..a.len {{\n out[i + 1] = a[i]\n }}\n}}"), "cannot modify `out[..]` inside a `par for`");
    err("fn f(a: [f64]) -> f64 {\n let mut t = 0.0\n par for i in 0..a.len {\n t += a[i]\n }\n t\n}", "cannot modify `t` inside a `par for`");
    err("fn f(v: mut Vec<i64>) {\n par for i in 0..3 {\n v.push(1)\n }\n}", "cannot modify `v` inside a `par for`");
}

#[test]
fn par_for_locals_are_private_to_each_iteration() {
    ok(&format!("{PAR} par for i in 0..a.len {{\n let mut t = a[i]\n t += 1.0\n out[i] = t\n }}\n}}"));
}

#[test]
fn par_chunks_gives_each_iteration_its_own_slice() {
    ok("fn f(out: mut [f64]) {\n par for c in par_chunks(mut out, 4) {\n c[0] = 0.0\n }\n}");
    // while the chunks are alive `out` is mutably borrowed
    err("fn f(out: mut [f64]) {\n par for c in par_chunks(mut out, 4) {\n out[0] = 1.0\n }\n}", "cannot modify `out[..]` inside a `par for`");
}

#[test]
fn parallel_reductions_only_read() {
    ok("fn f(a: [f64]) -> f64 { par sum(i in 0..a.len) a[i] * a[i] }");
    err("fn f(a: [f64], out: mut [f64]) -> f64 { par sum(i in 0..a.len) { out[i] = 1.0\n a[i] } }", "cannot modify `out[..]` inside a parallel reduction");
}

#[test]
fn on_blocks_cannot_modify_local_state() {
    err_m("let mut n = 0\non local {\n n += 1\n}", "cannot modify `n` inside an `on` block");
    ok_m("let n = 1\nlet r = on local { n + 1 }");
}

// ------------------------------------------------------------------ no cascades

#[test]
fn one_mistake_gives_one_error() {
    err_m("let a = vec[1]\neat(a)\nlook(a)\nprint(a.len)", "use of moved value `a`");
}

#[test]
fn errors_carry_a_note_pointing_at_the_other_location() {
    let src = m("let a = vec[1]\neat(a)\nprint(a.len)");
    let r = run(&src);
    assert_eq!(r.diagnostics.len(), 1);
    let d = &r.diagnostics[0];
    assert_eq!(d.notes.len(), 1, "{d:?}");
    let text = d.render(&src, "t.tau");
    assert!(text.contains("`a` moved here"), "{text}");
}

// ------------------------------------------------------------------ the realistic programs

#[test]
fn borrow_errors_are_only_reported_for_programs_that_type_check() {
    let r = run("fn main() { let x: i64 = \"s\"\n let a = vec[1]\n let b = a\n print(a.len) }");
    assert_eq!(r.diagnostics.len(), 1);
    assert!(r.diagnostics[0].message.contains("mismatched types"));
}

// ------------------------------------------------------------------ valid code must be accepted

#[test]
fn index_loops_that_read_and_write_the_same_vector() {
    ok("fn double(v: mut [i64]) {\n for i in 0..v.len {\n v[i] = v[i] * 2\n }\n}");
    ok("fn prefix(v: mut [i64]) {\n for i in 1..v.len {\n v[i] = v[i] + v[i - 1]\n }\n}");
    ok("fn fill(v: mut Vec<i64>, n: i64) {\n for i in 0..n {\n v.push(i)\n }\n}");
}

#[test]
fn swapping_elements() {
    ok("fn swap(v: mut [i64], i: usize, j: usize) {\n let t = v[i]\n v[i] = v[j]\n v[j] = t\n}");
    ok("fn rev(v: mut [i64]) {\n let mut i = 0\n let mut j = v.len - 1\n while i < j {\n let t = v[i]\n v[i] = v[j]\n v[j] = t\n i += 1\n j -= 1\n }\n}");
}

#[test]
fn reborrowing_a_mutable_parameter() {
    ok("fn inner(v: mut Vec<i64>) { v.push(1) }\nfn outer(v: mut Vec<i64>) {\n inner(mut v)\n inner(mut v)\n v.push(2)\n}");
    ok("fn look(v: [i64]) {}\nfn outer(v: mut Vec<i64>) {\n look(v)\n v.push(2)\n look(v)\n}");
}

#[test]
fn methods_that_mutate_their_own_fields() {
    let s = "struct Stack { items: Vec<i64>, count: i64 }\nimpl Stack {\n";
    ok(&format!("{s} fn push(mut self, x: i64) {{\n self.items.push(x)\n self.count += 1\n }}\n}}"));
    ok(&format!("{s} fn dup(mut self) {{\n let n = self.items.len\n self.items.push(n as i64)\n }}\n}}"));
    ok(&format!("{s} fn len2(self) -> usize {{ self.items.len + self.items.len }}\n}}"));
    ok(&format!("{s} fn reset(mut self) {{\n self.items = vec[]\n self.count = 0\n }}\n}}"));
}

#[test]
fn accumulating_into_locals() {
    ok("fn sum(v: [f64]) -> f64 {\n let mut t = 0.0\n for x in v {\n t += x\n }\n t\n}");
    ok("fn evens(v: [i64]) -> Vec<i64> {\n let mut out = Vec<i64>.new()\n for x in v {\n if x % 2 == 0 {\n out.push(x)\n }\n }\n out\n}");
    ok("fn count(v: [i64]) -> i64 {\n let mut n = 0\n let mut i = 0\n while i < v.len {\n if v[i] > 0 { n += 1 }\n i += 1\n }\n n\n}");
}

#[test]
fn building_and_using_values_in_sequence() {
    ok("fn f() -> i64 {\n let a = vec[1, 2, 3]\n let b = a.map(|x| x * 2)\n let c = b.filter(|x| x > 2)\n c.sum()\n}");
    ok("fn f() {\n let s = String.from(\"hi\")\n let t = s.to_string()\n print(s)\n print(t)\n}");
    ok("struct P { name: String, age: i64 }\nfn f() {\n let p = P(String.from(\"a\"), 3)\n let q = P(p.name, p.age + 1)\n print(q.age)\n}");
}

#[test]
fn early_returns_and_conditional_flow() {
    ok("fn f(v: Vec<i64>) -> usize {\n if v.is_empty() {\n return 0\n }\n v.len\n}");
    ok("fn f(own v: Vec<i64>, c: bool) -> Vec<i64> {\n if c {\n return v\n }\n v\n}");
    ok("fn f(own v: Vec<i64>, c: bool) -> usize {\n if c {\n let w = v\n return w.len\n }\n v.len\n}");
    ok("fn find(v: [i64], x: i64) -> Option<usize> {\n for i in 0..v.len {\n if v[i] == x { return Some(i) }\n }\n None\n}");
}

#[test]
fn shadowing_and_rebinding() {
    ok("fn f() {\n let a = vec[1]\n let a = a.map(|x| x + 1)\n print(a.len)\n}");
    ok("fn f() {\n let mut a = vec[1]\n a = a.map(|x| x + 1)\n a.push(3)\n}");
}

#[test]
fn nested_loops_over_the_same_data() {
    ok("fn f(v: [i64]) -> i64 {\n let mut t = 0\n for a in v {\n for b in v {\n t += a * b\n }\n }\n t\n}");
    ok("fn f(v: [f64], w: mut [f64]) {\n for i in 0..v.len {\n for j in 0..v.len {\n w[i] += v[j]\n }\n }\n}");
}

#[test]
fn passing_the_same_data_to_several_borrowing_parameters() {
    ok("fn two(a: [i64], b: [i64]) -> i64 { a[0] + b[0] }\nfn f(v: Vec<i64>) -> i64 { two(v, v) }");
    ok("fn dot(a: [f64], b: [f64]) -> f64 { par sum(i in 0..a.len) a[i] * b[i] }\nfn f(v: Vec<f64>) -> f64 { dot(v, v) }");
}

#[test]
fn matrix_style_code() {
    ok("fn f() {\n let a = Tensor<f64, [_, _]>.random(3, 3)\n let b = a @ a\n let c = a + b\n print(trace(c))\n print(trace(a))\n}");
    ok("fn f(a: Tensor<f64, [_, _]>) -> f64 { trace(a @ a) + trace(a) }");
}

#[test]
fn passing_closures_around() {
    ok("fn apply(f: fn(i64) -> i64, x: i64) -> i64 { f(x) }\nfn g() -> i64 {\n let k = 3\n apply(|x| x + k, 4) + apply(|x| x * k, 5)\n}");
    ok("fn g(v: [i64]) -> Vec<i64> {\n let k = 2\n v.map(|x| x * k)\n}");
}

#[test]
fn tasks_that_are_joined_before_the_data_changes() {
    ok("fn work(v: [i64]) -> i64 { v.len as i64 }\nfn f() {\n let mut data = vec[1, 2, 3]\n let h = spawn { work(data) }\n let r = h.join()\n data.push(4)\n let h2 = spawn { work(data) }\n h2.join()\n}");
}

#[test]
fn par_loops_over_borrowed_inputs() {
    ok("fn f(a: [f64], b: [f64], out: mut [f64]) {\n par for i in 0..a.len {\n out[i] = a[i] + b[i]\n }\n}");
    ok("fn f(a: [f64], k: f64, out: mut [f64]) {\n par for i in 0..a.len {\n out[i] = a[i] * k\n }\n}");
    ok("fn f(a: [f64]) -> f64 {\n let scale = 2.0\n par sum(i in 0..a.len) a[i] * scale\n}");
}

// ------------------------------------------------------------------ subtler unsafe programs

#[test]
fn views_stored_inside_other_values() {
    let s = "struct View { s: [i64] }\n";
    err(&format!("{s}fn main() {{\n let mut v = vec[1, 2]\n let w = View(v[0..1])\n v.push(1)\n print(w.s.len)\n}}"), "cannot borrow `v` as mutable");
    err("fn main() {\n let mut v = vec[1, 2]\n let o = Some(v[0..1])\n v.push(1)\n match o {\n Some(s) => print(s.len)\n None => print(0)\n }\n}", "cannot borrow `v` as mutable");
    err("fn main() {\n let mut v = vec[1, 2]\n let t = (v[0..1], 5)\n v.push(1)\n print(t.1)\n}", "");
}

#[test]
fn views_through_branches_and_loops() {
    err("fn main() {\n let mut v = vec[1]\n let w = vec[2]\n let s = if true { v[0..1] } else { w[0..1] }\n v.push(1)\n print(s.len)\n}", "cannot borrow `v` as mutable");
    err("fn main() {\n let mut v = vec[1]\n let s = v[0..1]\n loop {\n v.push(1)\n print(s.len)\n }\n}", "cannot borrow `v` as mutable");
    ok("fn main() {\n let mut v = vec[1]\n let w = vec[2]\n let mut s = v[0..1]\n s = w[0..1]\n v.push(1)\n print(s.len)\n}");
}

#[test]
fn methods_that_return_views_keep_the_receiver_borrowed() {
    err("struct S { items: Vec<i64> }\nimpl S {\n fn first(self) -> [i64] { self.items[0..1] }\n fn add(mut self, x: i64) { self.items.push(x) }\n}\nfn main() {\n let mut s = S(vec[1])\n let f = s.first()\n s.add(2)\n print(f.len)\n}", "cannot borrow `s` as mutable");
    ok("struct S { items: Vec<i64> }\nimpl S {\n fn first(self) -> [i64] { self.items[0..1] }\n fn add(mut self, x: i64) { self.items.push(x) }\n}\nfn main() {\n let mut s = S(vec[1])\n let f = s.first()\n print(f.len)\n s.add(2)\n}");
    err("fn main() {\n let mut t = String.from(\"a b\")\n let w = t.split_whitespace()\n t.push_str(\"c\")\n print(w.len)\n}", "cannot borrow `t` as mutable");
}

#[test]
fn writing_or_moving_a_borrowed_value() {
    err("fn main() {\n let mut v = vec[1, 2]\n let s = v[0..1]\n v[0] = 5\n print(s.len)\n}", "cannot assign to `v[..]` because it is borrowed");
    err_m("let v = vec[1]\nlet s = v[0..1]\neat(v)\nprint(s.len)", "cannot move out of `v` because it is borrowed");
    err_m("let v = vec[1]\nfor x in v {\n eat(v)\n}", "cannot move out of `v` because it is borrowed");
}

#[test]
fn escaping_borrows_of_owned_data() {
    err("fn f(own v: Vec<i64>) -> [i64] { v[0..1] }", "cannot return a value that borrows `v`");
    err("fn f() -> [i64] {\n let v = vec[1]\n let s = v[0..1]\n s\n}", "cannot return a value that borrows `v`");
    err("fn f() -> [i64] {\n let v = vec[1]\n if true { return v[0..1] }\n [1, 2]\n}", "cannot return a value that borrows `v`");
}

#[test]
fn moving_a_view_source_into_a_task() {
    err_m("let mut v = vec[1]\nlet h = spawn { v.len }\nlet w = v\nh.join()", "cannot move out of `v` because it is borrowed");
}

#[test]
fn a_closure_called_in_a_loop_keeps_its_captures_borrowed() {
    err_m("let mut v = vec[1]\nlet f = || v.len\nfor i in 0..2 {\n v.push(1)\n let n = f()\n}", "cannot borrow `v` as mutable");
    ok_m("let mut n = 0\nlet inc = || n += 1\ninc()\ninc()\nprint(n)");
}

#[test]
fn match_bindings_keep_the_scrutinee_borrowed() {
    err("fn f(own o: Option<String>) {\n match o {\n Some(s) => {\n o = None\n print(s)\n }\n None => print(0)\n }\n}", "cannot assign to `o` because it is borrowed");
}

#[test]
fn a_mutating_call_can_store_views_into_its_receiver() {
    err("fn main() {\n let mut v = vec[1, 2, 3]\n let mut views = Vec<[i64]>.new()\n views.push(v[0..2])\n v.push(4)\n print(views.len)\n}", "cannot borrow `v` as mutable");
    // ... but only while the container is still used
    ok("fn main() {\n let mut v = vec[1, 2, 3]\n let mut views = Vec<[i64]>.new()\n views.push(v[0..2])\n print(views.len)\n v.push(4)\n}");
    // writing into a plain slice copies data; it does not keep the source
    ok("fn copy_into(dst: mut [i64], src: [i64]) {}\nfn main() {\n let mut a = vec[1]\n let mut b = vec[2]\n copy_into(mut a, b)\n b.push(3)\n print(a.len)\n}");
}

#[test]
fn par_for_cannot_read_elements_other_iterations_write() {
    err("fn f(out: mut [f64]) {\n par for i in 1..out.len {\n out[i] = out[i - 1] + 1.0\n }\n}", "cannot read `out[..]` here");
    err("fn f(out: mut [f64]) {\n par for i in 0..out.len {\n out[0] = 1.0\n }\n}", "cannot modify `out[..]` inside a `par for`");
    // the same element is fine, and so is reading a different (read-only) slice
    ok("fn f(out: mut [f64]) {\n par for i in 0..out.len {\n out[i] = out[i] + 1.0\n }\n}");
    ok("fn f(a: [f64], out: mut [f64]) {\n par for i in 1..out.len {\n out[i] = a[i - 1] + a[i]\n }\n}");
    ok("fn f(a: [f64], out: mut [f64]) {\n par for i in 0..out.len {\n let mut t = 0.0\n for j in 0..a.len { t += a[j] }\n out[i] = t\n }\n}");
}

// ------------------------------------------------------------------ views must not outlive their data

#[test]
fn a_view_of_a_temporary_cannot_outlive_the_statement() {
    err("fn first(v: Vec<i64>) -> [i64] { v[0..1] }\nfn main() {\n let s = first(vec[1, 2, 3])\n print(s)\n}", "does not live long enough");
    err("fn main() {\n let w = \"a b\".to_string().split_whitespace()\n print(w)\n}", "does not live long enough");
    // used within the statement, or only copied out, it is fine
    ok("fn first(v: Vec<i64>) -> [i64] { v[0..1] }\nfn main() {\n print(first(vec[1, 2, 3]))\n let n = first(vec[4, 5]).len\n print(n)\n let c = first(vec[7, 8]).to_vec()\n print(c)\n}");
}

#[test]
fn a_view_of_an_inner_variable_cannot_outlive_its_block() {
    err_m("let mut s = vec[0]\nlet t = [0]\nlet mut v2 = [t[0..1]]\n{\n let v = vec[1, 2, 3]\n v2[0] = v[0..1]\n}\nprint(v2)", "does not live long enough");
    err_m("let r = {\n let v = vec[1, 2, 3]\n v[0..2]\n}\nprint(r)", "does not live long enough");
    ok_m("let r = {\n let v = vec[1, 2, 3]\n v[0..2].to_vec()\n}\nprint(r)");
    ok_m("let n = {\n let v = vec[1, 2, 3]\n v.len\n}\nprint(n)");
}

#[test]
fn tensor_methods_only_read_the_tensor() {
    ok_m("let a = Tensor<f64, [_]>.arange(4)\n let s = a.sum()\n let m = a.max()\n let b = a.map(|x| x + s)\n let r = a.reshape(2, 2)\n print(a)\n print(s + m)\n print(b)\n print(r)");
}

#[test]
fn parts_of_a_tensor_are_copies_that_nothing_keeps_borrowed() {
    ok_m("let mut m = Tensor<f64, [_, _]>.zeros(3, 3)\n let row = m[0, ..]\n m[1, ..] = row\n let col = m[.., 0]\n m[.., 1] = col\n m[2, 2] = 5.0\n print(row)\n print(col)\n print(m)");
    err_m("let mut m = Tensor<f64, [_, _]>.zeros(3, 3)\n let r = m[0, ..]\n let s = r\n print(r)", "use of moved value");
}

// ------------------------------------------------------------------ Box<T> and `own`

const TREE: &str = "\
enum Tree {
    Leaf
    Node(Box<Tree>, i64, Box<Tree>)
}
fn depth(t: Tree) -> i64 {
    match t {
        Leaf => 0
        Node(l, _, r) => 1 + max(depth(l), depth(r))
    }
}
";

fn with_tree(rest: &str) -> String {
    format!("{TREE}{rest}")
}

#[test]
fn boxes_are_looked_through_where_a_borrow_is_wanted() {
    ok(&with_tree("fn main() {\n let t = Node(Box.new(Leaf), 1, Box.new(Leaf))\n print(depth(t))\n print(depth(t))\n}\n"));
    // a field, a method and a match see through a box
    ok("struct P { x: i64 }\nfn main() {\n let b = Box.new(P(1))\n let v = Box.new(vec[1, 2])\n print(b.x + v.len as i64)\n match v { x => print(x) }\n}\n");
}

#[test]
fn a_box_is_moved_by_take_and_by_being_passed_on() {
    err("fn main() {\n let b = Box.new(vec[1])\n let v = b.take()\n print(v.len)\n print(b.len)\n}\n", "use of moved value");
    err("fn eat(own b: Box<i64>) {}\nfn main() {\n let b = Box.new(1)\n eat(b)\n eat(b)\n}\n", "use of moved value");
}

#[test]
fn match_own_moves_the_parts_out_of_an_owned_value() {
    // without `own`, the parts are views and cannot be moved
    err(
        &with_tree("fn f(own t: Tree) -> Tree {\n match t {\n  Leaf => Leaf\n  Node(l, _, _) => f(l.take())\n }\n}\nfn main() {}\n"),
        "it is a borrowed view of another value",
    );
    // with it they can
    ok(&with_tree("fn f(own t: Tree) -> Tree {\n match own t {\n  Leaf => Leaf\n  Node(l, x, r) => Node(Box.new(f(l.take())), x, r)\n }\n}\nfn main() {}\n"));
    // `t` is gone afterwards
    err(
        &with_tree("fn f(own t: Tree) -> i64 {\n let n = match own t {\n  Leaf => 0\n  Node(_, x, _) => x\n }\n n + depth(t)\n}\nfn main() {}\n"),
        "use of moved value",
    );
    // a borrowed parameter cannot be moved out of
    err(&with_tree("fn f(t: Tree) -> i64 {\n match own t {\n  Leaf => 0\n  Node(_, x, _) => x\n }\n}\nfn main() {}\n"), "only borrowed");
}

#[test]
fn own_copies_a_copy_value_and_moves_anything_else() {
    ok_m("let a = 5\n let b = own a\n print(a + b)");
    err_m("let v = vec[1]\n let w = own v\n print(v.len)", "use of moved value");
    ok_m("let v = vec[1]\n let w = own v\n print(w.len)");
}

// ------------------------------------------------------------------ iterators

#[test]
fn an_iterator_borrows_what_it_walks_over() {
    err_m("let mut v = vec[1, 2, 3]\n let it = v.iter()\n v.push(4)\n print(it.count())", "also borrowed");
    ok_m("let mut v = vec[1, 2, 3]\n let n = v.iter().count()\n v.push(n as i64)\n print(v.len)");
    // a chain holds what its closures capture
    err_m("let mut k = 1\n let v = vec[1, 2]\n let it = v.iter().map(|x| x + k)\n k = 5\n print(it.count())", "borrowed");
    ok_m("let k = 1\n let v = vec[1, 2]\n let it = v.iter().map(|x| x + k)\n print(k)\n print(it.count())");
}

#[test]
fn a_for_loop_uses_up_an_iterator_unless_it_is_borrowed_mut() {
    err_m("let v = vec[1, 2, 3]\n let it = v.iter()\n for x in it { print(x) }\n for x in it { print(x) }", "use of moved value");
    ok_m("let v = vec[1, 2, 3]\n let mut it = v.iter()\n for x in mut it { if x == 2 { break } }\n print(it.next())");
    // the items are owned: they do not keep the vector borrowed
    ok_m("let mut v = vec[1, 2, 3]\n let mut firsts = Vec.new()\n for x in v.iter() { firsts.push(x) }\n v.push(9)\n print(firsts)");
}

#[test]
fn an_iterator_cannot_outlive_what_it_walks_over() {
    err_m("let it = {\n let v = vec[1, 2, 3]\n v.iter()\n}\n print(it.count())", "does not live long enough");
    ok_m("let n = {\n let v = vec[1, 2, 3]\n v.iter().count()\n}\n print(n)");
}

// ------------------------------------------------------------------ Shared<T> and Mutex<T>

#[test]
fn handles_move_into_tasks_and_a_clone_is_another_handle() {
    ok_m("let s = Shared.new(vec[1, 2, 3])\n let c = s.clone()\n let t = spawn { let mine = own c\n mine.len }\n print(s.len)\n print(t.join().is_ok_or(0))".replace("print(t.join().is_ok_or(0))", "match t.join() {\n Ok(n) => print(n)\n Err(_) => print(0)\n }").as_str());
    err_m("let s = Shared.new(vec[1])\n let t = spawn { let mine = own s\n mine.len }\n print(s.len)\n match t.join() {\n Ok(n) => print(n)\n Err(_) => print(0)\n }", "use of moved value");
    // a task may use a handle without moving it (a read-only borrow, until the task is joined)
    ok_m("let s = Shared.new(vec[1, 2, 3])\n let t = spawn { s.len }\n match t.join() {\n Ok(n) => print(n)\n Err(_) => print(0)\n }\n print(s.len)");
}

#[test]
fn a_mutex_in_a_shared_handle_is_changed_through_a_read_borrow() {
    ok_m("let m = Shared.new(Mutex.new(0))\n let t = spawn { m.update(|x| x + 1) }\n match t.join() {\n Ok(_) => print(1)\n Err(_) => print(0)\n }\n par for i in 0..4 { m.update(|x| x + 1) }\n print(m.get())");
}

#[test]
fn a_shared_handle_and_a_mutex_are_moved() {
    err_m("let s = Shared.new(vec[1])\n let t = s\n print(s.len)", "use of moved value");
    err_m("let m = Mutex.new(1)\n let c = m.into_inner()\n print(m.get())", "use of moved value");
    ok_m("let s = Shared.new(vec[1])\n let t = s.clone()\n print(s.len + t.len)");
}

// ------------------------------------------------------------------ strings

#[test]
fn parts_of_a_string_view_it() {
    // the pieces borrow the text, so it cannot change while they are used
    err_m("let mut text = String.from(\"a,b\")\n let parts = text.split(\",\")\n text.push_str(\"c\")\n print(parts.len)", "also borrowed");
    err_m("let mut text = String.from(\" a \")\n let t = text.trim()\n text.push('x')\n print(t)", "also borrowed");
    err_m("let mut text = String.from(\"abc\")\n let first = text[0..1]\n text.clear()\n print(first)", "also borrowed");
    // owned results do not
    ok_m("let mut text = String.from(\"a,b\")\n let n = text.split(\",\").len\n let up = text.replace(\"a\", \"A\")\n text.push_str(\"c\")\n print(n)\n print(up)");
    // a piece cannot outlive the text
    err_m("let t = {\n let text = String.from(\" a \")\n text.trim()\n}\n print(t)", "does not live long enough");
    ok_m("let t = {\n let text = String.from(\" a \")\n text.trim().to_string()\n}\n print(t)");
}
