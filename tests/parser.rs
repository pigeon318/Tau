//! Structure tests: parse a snippet and compare its S-expression form.

use tau::sexp;
use tau::{parse_expr, parse_module};

fn e(src: &str) -> String {
    match parse_expr(src) {
        Ok(x) => sexp::expr(&x),
        Err(d) => panic!("failed to parse {src:?}: {}", d.render(src, "<test>")),
    }
}

fn check(src: &str, want: &str) {
    assert_eq!(e(src), want, "for source {src:?}");
}

#[test]
fn arithmetic_precedence() {
    check("a + b * c", "(+ a (* b c))");
    check("a * b + c", "(+ (* a b) c)");
    check("a - b - c", "(- (- a b) c)");
    check("a / b * c", "(* (/ a b) c)");
    check("a + b < c * d", "(< (+ a b) (* c d))");
    check("a || b && c", "(|| a (&& b c))");
    check("a == b && c != d", "(&& (== a b) (!= c d))");
}

#[test]
fn matmul_is_multiplicative_level() {
    check("a @ b * c", "(* (@ a b) c)");
    check("a + b @ c", "(+ a (@ b c))");
}

#[test]
fn power_is_right_associative_and_binds_tighter_than_unary_minus() {
    check("2^3^2", "(^ 2 (^ 3 2))");
    check("-x^2", "(neg (^ x 2))");
    check("2^-1", "(^ 2 (neg 1))");
    check("a * b^2", "(* a (^ b 2))");
    check("d.norm()^3", "(^ (method d norm) 3)");
}

#[test]
fn bitwise_operators() {
    check("a ^^ b", "(^^ a b)");
    check("a & b | c ^^ d", "(| (& a b) (^^ c d))");
    check("a << 2", "(<< a 2)");
    check("a >> 2", "(>> a 2)");
    check("a > b", "(> a b)");
    check("a >= b", "(>= a b)");
    check("~a", "(~ a)");
    check("!a", "(! a)");
}

#[test]
fn casts() {
    check("i as f64 * h", "(* (as i f64) h)");
    check("(b - a) / n as f64", "(/ (- b a) (as n f64))");
    check("x as usize < y", "(< (as x usize) y)");
    check("x as i32 as f64", "(as (as x i32) f64)");
    check("-x as f64", "(as (neg x) f64)");
}

#[test]
fn ranges() {
    check("0..n", "(.. 0 n)");
    check("0..=n", "(..= 0 n)");
    check("..n", "(.. _ n)");
    check("a..", "(.. a _)");
    check("a + 1..b * 2", "(.. (+ a 1) (* b 2))");
    check("a[1..3]", "(index a (.. 1 3))");
}

#[test]
fn postfix_chains() {
    check("f(a, b)", "(call f a b)");
    check("f()", "(call f)");
    check("x.y.z", "(. (. x y) z)");
    check("x.0", "(. x 0)");
    check("t.0.1", "(. (. t 0) 1)");
    check("a[i, j]", "(index a i j)");
    check("a[i][j]", "(index (index a i) j)");
    check("fs.read(p)?", "(? (method fs read p))");
    check("v.map(|x| x * 2).sum()", "(method (method v map (closure (x) (* x 2))) sum)");
    check("a.b(c).d", "(. (method a b c) d)");
}

#[test]
fn assignment() {
    check("x = 1", "(= x 1)");
    check("x += a * b", "(+= x (* a b))");
    check("a = b = c", "(= a (= b c))");
    check("p.pos += p.vel * dt", "(+= (. p pos) (* (. p vel) dt))");
    check("m[k] -= 1", "(-= (index m k) 1)");
    check("a @= b", "(@= a b)");
    check("a ^= 2", "(^= a 2)");
}

#[test]
fn literals() {
    check("42", "42");
    check("1_000", "1000");
    check("0xFF", "255");
    check("3.5", "3.5");
    check("5u8", "5u8");
    check("2.0f32", "2.0f32");
    check("true", "true");
    check("()", "(tuple)");
    check("(1, 2)", "(tuple 1 2)");
    check("(1,)", "(tuple 1)");
    check("(1)", "1");
    check("[1, 2, 3]", "(array 1 2 3)");
    check("[]", "(array)");
    check("[0.0; 100]", "(repeat 0.0 100)");
    check("3 + 4i", "(+ 3 4.0i)");
}

#[test]
fn string_interpolation() {
    check(r#""hello""#, r#""hello""#);
    check(r#""x = {x}""#, r#""x = {x}""#);
    check(r#""sum {a + b * 2}!""#, r#""sum {(+ a (* b 2))}!""#);
    check(r#""{f(x)} and {y.z}""#, r#""{(call f x)} and {(. y z)}""#);
}

#[test]
fn generics_in_expressions() {
    // generic application: type args followed by `.` or `(`
    check("Map<String, usize>.new()", "(method Map<String, usize> new)");
    check(
        "Tensor<f64, [_, _]>.random(100, 50)",
        "(method Tensor<f64, [_, _]> random 100 50)",
    );
    check("alloc<u8>(n)", "(call alloc<u8> n)");
    check(
        "Vec<Vec<f64>>.new()",
        "(method Vec<Vec<f64>> new)",
    );
    // comparisons are not mistaken for generics
    check("a < b", "(< a b)");
    check("a < b && c > d", "(&& (< a b) (> c d))");
    check("i < n", "(< i n)");
    check("N < M", "(< N M)");
    check("a < (b)", "(< a b)");
    check("x < y + 1", "(< x (+ y 1))");
}

#[test]
fn call_site_mut() {
    check("scale(mut data, 2.0)", "(call scale (mut data) 2.0)");
    check("f(mut a[0])", "(call f (mut (index a 0)))");
}

#[test]
fn parallel_constructs() {
    check(
        "par sum(i in 0..n) a[i] * b[i]",
        "(par-sum i (.. 0 n) (* (index a i) (index b i)))",
    );
    check(
        "par sum(i in 1..n) f(a + i as f64 * h)",
        "(par-sum i (.. 1 n) (call f (+ a (* (as i f64) h))))",
    );
    check("par for i in 0..n { out[i] = f(i) }", "(par-for i (.. 0 n) (block (= (index out i) (call f i))))");
    check("spawn { heavy(data) }", "(spawn (block (call heavy data)))");
    check("on lab { f(x) }", "(on lab (block (call f x)))");
    check("on local { f(x) }", "(on local (block (call f x)))");
}

#[test]
fn closures() {
    check("|x| x * 2", "(closure (x) (* x 2))");
    check("|a, b| a + b", "(closure (a b) (+ a b))");
    check("|a, b| { a + b }", "(closure (a b) (block (+ a b)))");
    check("|| 42", "(closure () 42)");
    check("|x: f64| x", "(closure (x:f64) x)");
    check("f(|x| sin(x), 0.0)", "(call f (closure (x) (call sin x)) 0.0)");
    check("|(a, b)| a + b", "(closure ((tuple a b)) (+ a b))");
}

#[test]
fn newline_continuation() {
    check("a +\n b", "(+ a b)");
    check("x\n  .foo()\n  .bar()", "(method (method x foo) bar)");
    check("f(a,\n  b,\n  c)", "(call f a b c)");
    check("[1,\n 2,\n 3]", "(array 1 2 3)");
}

#[test]
fn if_expressions() {
    check("if a { b } else { c }", "(if a (block b) (block c))");
    check("if a { b }", "(if a (block b))");
    check(
        "if a { b } else if c { d } else { e }",
        "(if a (block b) (if c (block d) (block e)))",
    );
    // `else` on the line after the closing brace
    check("if a { b }\nelse { c }", "(if a (block b) (block c))");
    check("if x > 0 { 1 } else { -1 }", "(if (> x 0) (block 1) (block (neg 1)))");
}

#[test]
fn loops_and_flow() {
    check("for i in 0..10 { f(i) }", "(for i (.. 0 10) (block (call f i)))");
    check(
        "for j in 0..n where j != i { f(j) }",
        "(for j (.. 0 n) (where (!= j i)) (block (call f j)))",
    );
    check("for b in mut bodies { g(b) }", "(for b (mut bodies) (block (call g b)))");
    check("for (w, n) in top { p(w) }", "(for (tuple w n) top (block (call p w)))");
    check("while a < b { a += 1 }", "(while (< a b) (block (+= a 1)))");
    check("loop { break }", "(loop (block (break)))");
    check("{ return }", "(block (return))");
    check("{ return 1 + 2 }", "(block (return (+ 1 2)))");
    check("{ continue }", "(block (continue))");
}

#[test]
fn match_expressions() {
    check(
        "match s {\n  Circle(r) => r\n  Rect(w, h) => w * h\n  _ => 0\n}",
        "(match s (arm (Circle r) => r) (arm (Rect w h) => (* w h)) (arm _ => 0))",
    );
    check(
        "match (a, b) {\n  (0, 0) => 1\n  (0, _) => 2\n}",
        "(match (tuple a b) (arm (tuple 0 0) => 1) (arm (tuple 0 _) => 2))",
    );
    check(
        "match n {\n  1 | 2 => a\n  4..=9 => b\n  x if x < 0 => c\n  _ => d\n}",
        "(match n (arm (or 1 2) => a) (arm (..= 4 9) => b) (arm x if (< x 0) => c) (arm _ => d))",
    );
    // commas between arms are allowed
    check("match n { 1 => a, 2 => b }", "(match n (arm 1 => a) (arm 2 => b))");
    check("match o {\n  Some(x) => x\n  None => 0\n}", "(match o (arm (Some x) => x) (arm None => 0))");
    check("match n {\n -1 => a\n _ => b\n}", "(match n (arm (neg 1) => a) (arm _ => b))");
}

#[test]
fn blocks_and_statements() {
    check(
        "{\n  let mut x = 1\n  x += 2\n  x\n}",
        "(block (let (mut x) 1) (+= x 2) x)",
    );
    check("{ let x: f64 = 1.0 }", "(block (let x : f64 1.0))");
    check("{ let (a, b) = f() }", "(block (let (tuple a b) (call f)))");
    check("{ let x }", "(block (let x))");
    check("unsafe { p[i] = v }", "(unsafe (block (= (index p i) v)))");
}

// ------------------------------------------------------------------ items

fn module(src: &str) -> tau::ast::Module {
    match parse_module(src) {
        Ok(m) => m,
        Err(d) => panic!("failed to parse:\n{}", d.render(src, "<test>")),
    }
}

fn first_fn(m: &tau::ast::Module) -> &tau::ast::FnDecl {
    match &m.items[0].kind {
        tau::ast::ItemKind::Fn(f) => f,
        other => panic!("expected fn, got {other:?}"),
    }
}

#[test]
fn function_signature() {
    use tau::ast::ParamMode::*;
    let m = module("pub fn f<T: Number + Display, U>(a: T, v: mut [f64], own w: Vec<U>) -> f64 { 1.0 }");
    assert!(m.items[0].public);
    let f = first_fn(&m);
    assert_eq!(f.name.name, "f");
    assert_eq!(f.generics.len(), 2);
    assert_eq!(f.generics[0].bounds.len(), 2);
    assert_eq!(f.generics[1].bounds.len(), 0);
    let modes: Vec<_> = f.params.iter().map(|p| p.mode).collect();
    assert_eq!(modes, vec![Borrow, Mut, Own]);
    assert_eq!(sexp::ty(f.params[1].ty.as_ref().unwrap()), "[f64]");
    assert_eq!(sexp::ty(f.params[2].ty.as_ref().unwrap()), "Vec<U>");
    assert_eq!(sexp::ty(f.ret.as_ref().unwrap()), "f64");
}

#[test]
fn self_parameters() {
    use tau::ast::ItemKind;
    use tau::ast::ParamMode::*;
    let m = module("impl V {\n  fn a(self) {}\n  fn b(mut self, x: f64) {}\n  fn c(own self) {}\n}");
    let ItemKind::Impl(imp) = &m.items[0].kind else { panic!() };
    let modes: Vec<_> = imp
        .items
        .iter()
        .map(|i| match &i.kind {
            ItemKind::Fn(f) => f.params[0].mode,
            _ => panic!(),
        })
        .collect();
    assert_eq!(modes, vec![Borrow, Mut, Own]);
    assert!(matches!(&imp.items[0].kind, ItemKind::Fn(f) if f.params[0].ty.is_none()));
}

#[test]
fn types() {
    let m = module(
        "fn f(a: (i32, f64), b: [f64; 3], c: Tensor<f64, [3, _, N]>, d: fn(f64) -> f64, e: Vec<Vec<f64>>, g: ()) {}",
    );
    let f = first_fn(&m);
    let tys: Vec<String> = f
        .params
        .iter()
        .map(|p| sexp::ty(p.ty.as_ref().unwrap()))
        .collect();
    assert_eq!(
        tys,
        vec![
            "(i32, f64)",
            "[f64; 3]",
            "Tensor<f64, [3, _, N]>",
            "fn(f64) -> f64",
            "Vec<Vec<f64>>",
            "()",
        ]
    );
}

#[test]
fn struct_enum_trait_impl() {
    use tau::ast::ItemKind;
    let m = module(
        "struct P { pub x: f64, y: f64 }\n\
         struct Q {\n  a: i32\n  b: i32\n}\n\
         enum Shape {\n  Circle(f64)\n  Rect(f64, f64)\n  Empty\n}\n\
         trait Number {\n  fn zero() -> Self\n  fn add(self, o: Self) -> Self\n}\n\
         impl Add for V { fn add(self, o: V) -> V { o } }\n\
         impl<T> W<T> { fn get(self) -> T { self.0 } }",
    );
    let ItemKind::Struct(p) = &m.items[0].kind else { panic!() };
    assert_eq!(p.fields.len(), 2);
    assert!(p.fields[0].public && !p.fields[1].public);
    let ItemKind::Struct(q) = &m.items[1].kind else { panic!() };
    assert_eq!(q.fields.len(), 2);
    let ItemKind::Enum(en) = &m.items[2].kind else { panic!() };
    let arity: Vec<usize> = en.variants.iter().map(|v| v.fields.len()).collect();
    assert_eq!(arity, vec![1, 2, 0]);
    let ItemKind::Trait(t) = &m.items[3].kind else { panic!() };
    assert_eq!(t.items.len(), 2);
    assert!(matches!(&t.items[0].kind, ItemKind::Fn(f) if f.body.is_none()));
    let ItemKind::Impl(i) = &m.items[4].kind else { panic!() };
    assert_eq!(sexp::ty(i.trait_ref.as_ref().unwrap()), "Add");
    assert_eq!(sexp::ty(&i.self_ty), "V");
    let ItemKind::Impl(i2) = &m.items[5].kind else { panic!() };
    assert!(i2.trait_ref.is_none());
    assert_eq!(i2.generics.len(), 1);
    assert_eq!(sexp::ty(&i2.self_ty), "W<T>");
}

#[test]
fn use_declarations() {
    use tau::ast::{ItemKind, UseKind};
    let m = module(
        "use math.linalg\nuse math.fft.{fft, ifft}\nuse mypkg.tools as t\nuse std.io.*\nuse a.{b.c, d}",
    );
    let tree = |i: usize| match &m.items[i].kind {
        ItemKind::Use(t) => t,
        _ => panic!(),
    };
    let path = |t: &tau::ast::UseTree| {
        t.path.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(".")
    };
    assert_eq!(path(tree(0)), "math.linalg");
    assert!(matches!(tree(0).kind, UseKind::Simple { alias: None }));
    assert_eq!(path(tree(1)), "math.fft");
    match &tree(1).kind {
        UseKind::Group(g) => assert_eq!(g.len(), 2),
        _ => panic!(),
    }
    assert!(matches!(&tree(2).kind, UseKind::Simple { alias: Some(a) } if a.name == "t"));
    assert!(matches!(tree(3).kind, UseKind::Glob));
    match &tree(4).kind {
        UseKind::Group(g) => assert_eq!(path(&g[0]), "b.c"),
        _ => panic!(),
    }
}

#[test]
fn consts_aliases_attributes() {
    use tau::ast::ItemKind;
    let m = module(
        "const G: f64 = 6.674e-11\ntype Grid = Tensor<f64, [_, _]>\n#[test]\nfn t() {}\n#[inline(always)]\n#[doc]\npub fn u() {}",
    );
    assert!(matches!(&m.items[0].kind, ItemKind::Const(c) if c.name.name == "G"));
    assert!(matches!(&m.items[1].kind, ItemKind::TypeAlias(t) if t.name.name == "Grid"));
    assert_eq!(m.items[2].attrs.len(), 1);
    assert_eq!(m.items[2].attrs[0].name.name, "test");
    assert_eq!(m.items[3].attrs.len(), 2);
    assert_eq!(m.items[3].attrs[0].args.len(), 1);
    assert!(m.items[3].public);
}

#[test]
fn cluster_declaration() {
    use tau::ast::ItemKind;
    let m = module("cluster lab = nodes [\"a:1\", \"b:2\"]");
    let ItemKind::Cluster(c) = &m.items[0].kind else { panic!() };
    assert_eq!(c.name.name, "lab");
    assert_eq!(c.nodes.len(), 2);
}

#[test]
fn nested_items_in_blocks() {
    let m = module("fn outer() {\n  const K: i64 = 3\n  fn inner() -> i64 { K }\n  inner()\n}");
    let f = first_fn(&m);
    let body = f.body.as_ref().unwrap();
    assert_eq!(body.stmts.len(), 3);
    assert!(matches!(body.stmts[0], tau::ast::Stmt::Item(_)));
    assert!(matches!(body.stmts[1], tau::ast::Stmt::Item(_)));
}

#[test]
fn spans_cover_the_source() {
    let src = "fn f() -> i64 { a + b * c }";
    let m = module(src);
    assert_eq!(m.items[0].span.start, 0);
    assert_eq!(m.items[0].span.end, src.len());
    let e = parse_expr("foo(a, b)").unwrap();
    assert_eq!((e.span.start, e.span.end), (0, 9));
}
