//! Multi-file programs and packages: loading, name resolution across modules, privacy, `tau.toml`,
//! path dependencies, and the project commands. Every program runs on the interpreter and (when a C
//! compiler is available) natively, and the two must agree.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tau::interp::Outcome;
use tau::project::{self, LoadOptions};

// ------------------------------------------------------------------ helpers

/// A folder in the system's temp directory that is removed when it goes out of scope.
struct Tmp(PathBuf);

static COUNTER: AtomicUsize = AtomicUsize::new(0);

impl Tmp {
    fn new(files: &[(&str, &str)]) -> Tmp {
        let dir = std::env::temp_dir().join(format!("tau-modules-test-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        let t = Tmp(dir.canonicalize().unwrap());
        for (path, text) in files {
            t.write(path, text);
        }
        t
    }
    fn write(&self, path: &str, text: &str) {
        let p = self.0.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    fn path(&self, p: &str) -> PathBuf {
        self.0.join(p)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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

fn load(entry: &Path) -> tau::Workspace {
    match project::load(entry, LoadOptions::default()) {
        Ok(w) => w,
        Err(e) => panic!("could not load {}: {}", entry.display(), e.render()),
    }
}

/// What the interpreter prints (panics if the program is rejected).
fn interpret(ws: &tau::Workspace) -> String {
    let cap = Capture(Arc::new(Mutex::new(Vec::new())));
    let r = tau::run_workspace(ws, Box::new(cap.clone()), u64::MAX);
    assert!(r.diagnostics.is_empty(), "rejected:\n{}", r.diagnostics.iter().map(|d| ws.render(d)).collect::<Vec<_>>().join("\n"));
    assert!(matches!(r.outcome, Some(Outcome::Ok)), "{:?}", r.outcome);
    let out = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
    out
}

/// Run a program both ways and require the same output; returns it.
fn run_both(entry: &Path) -> String {
    let ws = load(entry);
    let want = interpret(&ws);
    if tau::native::find_cc().is_some() {
        let got = tau::native::run_workspace_captured(&ws).unwrap_or_else(|e| panic!("native: {e:?}"));
        assert_eq!(got.code, 0, "native exited with {} ({})", got.code, got.stderr);
        assert_eq!(got.stdout, want, "native and interpreter disagree");
    }
    want
}

/// The error messages (rendered) of a program that is rejected.
fn rejected(entry: &Path) -> String {
    let ws = match project::load(entry, LoadOptions::default()) {
        Ok(w) => w,
        Err(e) => return e.render(),
    };
    let r = ws.analyze();
    assert!(!r.diagnostics.is_empty(), "the program was accepted");
    r.diagnostics.iter().map(|d| ws.render(d)).collect::<Vec<_>>().join("\n")
}

// ------------------------------------------------------------------ using other files

#[test]
fn a_program_can_use_the_files_next_to_it() {
    let t = Tmp::new(&[
        ("main.tau", "use util\nuse geo.shapes.{Circle, area}\n\nfn main() {\n    print(util.double(21))\n    let c = Circle(2.0)\n    print(area(c))\n}\n"),
        ("util.tau", "pub fn double(x: i64) -> i64 { x * 2 }\n"),
        ("geo/mod.tau", "pub fn unit() -> f64 { 1.0 }\n"),
        ("geo/shapes.tau", "pub struct Circle { pub r: f64 }\npub fn area(c: Circle) -> f64 { 3.0 * c.r * c.r }\n"),
    ]);
    assert_eq!(run_both(&t.path("main.tau")), "42\n12.0\n");
}

#[test]
fn all_the_ways_of_importing() {
    let t = Tmp::new(&[
        (
            "main.tau",
            "use lib.{one, two as deux}\nuse lib.Pair\nuse more.*\nuse lib as l\n\nfn main() {\n    print(one() + deux())\n    print(l.one())\n    let p = Pair(1, 2)\n    print(p.sum())\n    print(extra())\n    print(l.two())\n}\n",
        ),
        ("lib.tau", "pub fn one() -> i64 { 1 }\npub fn two() -> i64 { 2 }\npub struct Pair { pub a: i64, pub b: i64 }\nimpl Pair {\n    pub fn sum(self) -> i64 { self.a + self.b }\n}\n"),
        ("more.tau", "pub fn extra() -> i64 { 40 }\nfn hidden() -> i64 { 0 }\n"),
    ]);
    assert_eq!(run_both(&t.path("main.tau")), "3\n1\n3\n40\n2\n");
}

#[test]
fn modules_can_use_each_other_and_even_each_other_in_a_circle() {
    let t = Tmp::new(&[
        ("main.tau", "use a\nfn main() {\n    print(a.even(10))\n    print(a.even(7))\n}\n"),
        ("a.tau", "use b\npub fn even(n: i64) -> bool {\n    if n == 0 { true } else { b.odd(n - 1) }\n}\n"),
        ("b.tau", "use a\npub fn odd(n: i64) -> bool {\n    if n == 0 { false } else { a.even(n - 1) }\n}\n"),
    ]);
    assert_eq!(run_both(&t.path("main.tau")), "true\nfalse\n");
}

#[test]
fn imports_are_from_the_root_of_the_package_not_relative_to_the_file() {
    let t = Tmp::new(&[
        ("main.tau", "use shapes.circle\nfn main() {\n    print(circle.area(2.0))\n}\n"),
        ("shapes/mod.tau", "pub fn name() -> str { \"shapes\" }\n"),
        ("shapes/circle.tau", "use shapes\nuse units\npub fn area(r: f64) -> f64 { units.pi() * r * r }\npub fn label() -> str { shapes.name() }\n"),
        ("units.tau", "pub fn pi() -> f64 { 3.0 }\n"),
    ]);
    assert_eq!(run_both(&t.path("main.tau")), "12.0\n");
}

#[test]
fn same_names_in_different_modules_do_not_clash() {
    let t = Tmp::new(&[
        (
            "main.tau",
            "use a\nuse b\n\nstruct Point { x: i64 }\n\nfn area(p: Point) -> i64 { p.x }\nconst K: i64 = 100\n\nfn main() {\n    print(area(Point(1)))\n    print(a.area(a.Point(2.5)))\n    print(b.area(b.Point(3, 4)))\n    print(a.K + b.K + K)\n    print(a.describe(a.Tag.One))\n    print(b.describe(b.Tag.Two(7)))\n}\n",
        ),
        ("a.tau", "pub struct Point { pub x: f64 }\npub const K: i64 = 1\npub enum Tag { One\n Two }\npub fn area(p: Point) -> f64 { p.x * 2.0 }\npub fn describe(t: Tag) -> str {\n    match t {\n        One => \"a.One\"\n        Two => \"a.Two\"\n    }\n}\n"),
        ("b.tau", "pub struct Point { pub x: i64, pub y: i64 }\npub const K: i64 = 10\npub enum Tag { One\n Two(i64) }\npub fn area(p: Point) -> i64 { p.x * p.y }\npub fn describe(t: Tag) -> i64 {\n    match t {\n        One => 0\n        Two(n) => n\n    }\n}\n"),
    ]);
    assert_eq!(run_both(&t.path("main.tau")), "1\n5.0\n12\n111\na.One\n7\n");
}

#[test]
fn generics_traits_closures_and_constants_work_across_modules() {
    let t = Tmp::new(&[
        (
            "main.tau",
            "use vec3.Vec3\nuse algo\nuse algo.Shape\nuse consts.SCALE\n\nstruct Sq { s: f64 }\n\nimpl Shape for Sq {\n    fn area(self) -> f64 { self.s * self.s }\n}\n\nfn main() {\n    let a = Vec3(1.0, 2.0, 3.0)\n    let b = Vec3(1.0, 1.0, 1.0)\n    let c = a + b\n    print(c.dot(c))\n    print(algo.twice(9))\n    print(algo.twice(0.75))\n    print(algo.total_area(Sq(2.0), Sq(3.0)))\n    print(algo.apply(|x| x * SCALE, 7))\n}\n",
        ),
        ("vec3.tau", "pub struct Vec3 { pub x: f64, pub y: f64, pub z: f64 }\nimpl Add for Vec3 {\n    fn add(self, o: Vec3) -> Vec3 { Vec3(self.x + o.x, self.y + o.y, self.z + o.z) }\n}\nimpl Vec3 {\n    pub fn dot(self, o: Vec3) -> f64 { self.x * o.x + self.y * o.y + self.z * o.z }\n}\n"),
        ("algo.tau", "pub trait Shape {\n    fn area(self) -> f64\n}\npub fn twice<T: Num>(x: T) -> T { x + x }\npub fn total_area<A: Shape, B: Shape>(a: A, b: B) -> f64 { a.area() + b.area() }\npub fn apply(f: fn(i64) -> i64, x: i64) -> i64 { f(x) }\n"),
        ("consts.tau", "pub const SCALE: i64 = 6\n"),
    ]);
    assert_eq!(run_both(&t.path("main.tau")), "29.0\n18\n1.5\n13.0\n42\n");
}

#[test]
fn tests_in_other_modules_are_found_when_asked_for() {
    let t = Tmp::new(&[
        ("tau.toml", "[package]\nname = \"p\"\n"),
        ("src/main.tau", "use util\nfn main() {\n    print(util.double(2))\n}\n\n#[test]\nfn main_test() {\n    assert_eq(1, 1)\n}\n"),
        ("src/util.tau", "pub fn double(x: i64) -> i64 { x * 2 }\n\n#[test]\nfn doubles() {\n    assert_eq(double(4), 8)\n}\n\n#[test]\nfn broken() {\n    assert_eq(double(1), 3)\n}\n"),
        ("src/orphan.tau", "#[test]\nfn lonely() {\n    assert(true)\n}\n"),
    ]);
    let ws = project::load(&t.0, LoadOptions { all_modules: true }).unwrap_or_else(|e| panic!("{}", e.render()));
    let r = tau::test_workspace(&ws, None, u64::MAX);
    assert!(r.diagnostics.is_empty());
    let mut names: Vec<(String, bool)> = r.results.iter().map(|t| (t.name.clone(), t.passed)).collect();
    names.sort();
    assert_eq!(
        names,
        vec![("main_test".to_string(), true), ("orphan::lonely".to_string(), true), ("util::broken".to_string(), false), ("util::doubles".to_string(), true)]
    );
    // a filter matches the qualified name
    let r = tau::test_workspace(&ws, Some("util::"), u64::MAX);
    assert_eq!(r.results.len(), 2);
}

// ------------------------------------------------------------------ errors

#[test]
fn errors_name_the_file_they_are_in() {
    let t = Tmp::new(&[
        ("main.tau", "use util\nfn main() {\n    print(util.f())\n}\n"),
        ("util.tau", "pub fn f() -> i64 {\n    \"not a number\"\n}\n"),
    ]);
    let e = rejected(&t.path("main.tau"));
    assert!(e.contains("mismatched types"), "{e}");
    assert!(e.contains("util.tau:2:5"), "{e}");
    assert!(e.contains("\"not a number\""), "{e}");
}

#[test]
fn a_syntax_error_in_a_module_names_that_file() {
    let t = Tmp::new(&[("main.tau", "use util\nfn main() {}\n"), ("util.tau", "pub fn f( {\n")]);
    let e = rejected(&t.path("main.tau"));
    assert!(e.contains("util.tau:1"), "{e}");
}

#[test]
fn borrow_errors_in_other_modules_are_found() {
    let t = Tmp::new(&[
        ("main.tau", "use util\nfn main() {\n    util.f()\n}\n"),
        ("util.tau", "pub fn f() {\n    let v = vec[1, 2]\n    let w = v\n    print(v.len)\n}\n"),
    ]);
    let e = rejected(&t.path("main.tau"));
    assert!(e.contains("util.tau"), "{e}");
}

#[test]
fn a_missing_module_is_explained() {
    let t = Tmp::new(&[("main.tau", "use missing\nfn main() {}\n")]);
    let e = rejected(&t.path("main.tau"));
    assert!(e.contains("unresolved import `missing`"), "{e}");
    assert!(e.contains("missing.tau"), "{e}");
}

#[test]
fn module_names_are_case_sensitive_on_every_system() {
    // on Windows and macOS `Util.tau` would be found for `use util` by the file system
    let t = Tmp::new(&[("main.tau", "use util\nfn main() {}\n"), ("Util.tau", "pub fn f() {}\n")]);
    let e = rejected(&t.path("main.tau"));
    assert!(e.contains("unresolved import `util`"), "{e}");
}

#[test]
fn private_items_cannot_be_used_from_other_modules() {
    let t = Tmp::new(&[
        (
            "main.tau",
            "use shapes\nuse shapes.{Circle, secret}\n\nfn main() {\n    let c = Circle(1.0)\n    print(shapes.helper())\n    print(c.r)\n    let k = shapes.make(2.0)\n    print(k.area())\n    print(k.hidden())\n    print(k.r)\n    let w = shapes.Wrap(1.0)\n}\n",
        ),
        (
            "shapes.tau",
            "pub struct Circle { pub r: f64 }\npub struct Wrap { r: f64 }\nfn helper() -> i64 { 1 }\nfn secret() -> i64 { 2 }\npub fn make(r: f64) -> Wrap { Wrap(r) }\nimpl Wrap {\n    pub fn area(self) -> f64 { 3.0 * self.r * self.r }\n    fn hidden(self) -> f64 { self.r }\n}\n",
        ),
    ]);
    let e = rejected(&t.path("main.tau"));
    assert!(e.contains("`secret` is private to the module `shapes`"), "{e}");
    assert!(e.contains("`helper` is private to the module `shapes`"), "{e}");
    assert!(e.contains("the method `hidden` is private"), "{e}");
    assert!(e.contains("the field `r` of `Wrap` is private"), "{e}");
    assert!(e.contains("cannot build `Wrap` here: its field `r` is private"), "{e}");
    // what is public is fine: `Circle(1.0)`, `c.r`, `k.area()`
    assert_eq!(e.matches("error:").count(), 5, "{e}");
}

#[test]
fn private_types_traits_and_constants() {
    let t = Tmp::new(&[
        ("main.tau", "use m\nuse m.Hidden\n\nfn main() {\n    let x: m.Secret = m.make()\n    print(m.LIMIT)\n}\n"),
        ("m.tau", "struct Hidden { x: i64 }\nstruct Secret { x: i64 }\nconst LIMIT: i64 = 3\npub fn make() -> i64 { 1 }\n"),
    ]);
    let e = rejected(&t.path("main.tau"));
    assert!(e.contains("`Hidden` is private"), "{e}");
    assert!(e.contains("`Secret` is private"), "{e}");
    assert!(e.contains("`LIMIT` is private"), "{e}");
}

#[test]
fn what_a_module_imported_is_not_reexported() {
    let t = Tmp::new(&[
        ("main.tau", "use a\nuse a.helper\nfn main() {}\n"),
        ("a.tau", "use b.helper\npub fn x() -> i64 { helper() }\n"),
        ("b.tau", "pub fn helper() -> i64 { 1 }\n"),
    ]);
    let e = rejected(&t.path("main.tau"));
    assert!(e.contains("unresolved import `a.helper`"), "{e}");
}

#[test]
fn glob_imports_bring_in_only_public_items() {
    let t = Tmp::new(&[
        ("main.tau", "use m.*\nfn main() {\n    print(shown())\n    print(hidden())\n}\n"),
        ("m.tau", "pub fn shown() -> i64 { 1 }\nfn hidden() -> i64 { 2 }\n"),
    ]);
    let e = rejected(&t.path("main.tau"));
    assert!(e.contains("cannot find value `hidden`"), "{e}");
    assert_eq!(e.matches("error:").count(), 1, "{e}");
}

#[test]
fn a_module_can_use_its_own_private_items() {
    let t = Tmp::new(&[
        ("main.tau", "use m\nfn main() {\n    print(m.run())\n}\n"),
        ("m.tau", "struct Inner { x: i64 }\nfn make() -> Inner { Inner(5) }\nimpl Inner {\n    fn get(self) -> i64 { self.x }\n}\npub fn run() -> i64 { make().get() }\n"),
    ]);
    assert_eq!(run_both(&t.path("main.tau")), "5\n");
}

// ------------------------------------------------------------------ packages

const LIB_MANIFEST: &str = "[package]\nname = \"fastmath\"\nversion = \"0.2.0\"\n";

fn fastmath_files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("fastmath/tau.toml", LIB_MANIFEST),
        ("fastmath/src/lib.tau", "use vec\npub fn version() -> str { \"0.2.0\" }\npub fn norm2(x: f64, y: f64) -> f64 { vec.dot(x, y, x, y) }\n"),
        ("fastmath/src/vec.tau", "pub fn dot(a: f64, b: f64, c: f64, d: f64) -> f64 { a * c + b * d }\n"),
    ]
}

#[test]
fn a_program_can_depend_on_a_package_in_a_folder() {
    let mut files = fastmath_files();
    files.push(("app/tau.toml", "[package]\nname = \"app\"\n\n[dependencies]\nfastmath = { path = \"../fastmath\" }\n"));
    files.push(("app/src/main.tau", "use fastmath\nuse fastmath.vec\n\nfn main() {\n    print(fastmath.version())\n    print(fastmath.norm2(3.0, 4.0))\n    print(vec.dot(1.0, 2.0, 3.0, 4.0))\n}\n"));
    let t = Tmp::new(&files);
    assert_eq!(run_both(&t.path("app")), "0.2.0\n25.0\n11.0\n");
    // the entry can also be named by its file or by tau.toml
    assert_eq!(run_both(&t.path("app/src/main.tau")), "0.2.0\n25.0\n11.0\n");
    assert_eq!(run_both(&t.path("app/tau.toml")), "0.2.0\n25.0\n11.0\n");
}

#[test]
fn a_dependency_uses_its_own_modules_and_dependencies() {
    let t = Tmp::new(&[
        ("base/tau.toml", "[package]\nname = \"base\"\n"),
        ("base/src/lib.tau", "pub fn id() -> i64 { 7 }\n"),
        ("mid/tau.toml", "[package]\nname = \"mid\"\n\n[dependencies]\nbase = { path = \"../base\" }\n"),
        ("mid/src/lib.tau", "use base\nuse helper\npub fn value() -> i64 { base.id() + helper.one() }\n"),
        ("mid/src/helper.tau", "pub fn one() -> i64 { 1 }\n"),
        ("app/tau.toml", "[package]\nname = \"app\"\n\n[dependencies]\nmid = { path = \"../mid\" }\nbase = { path = \"../base\" }\n"),
        // the program also has a module called `helper`: it is not the dependency's
        ("app/src/main.tau", "use mid\nuse base\nuse helper\nfn main() {\n    print(mid.value())\n    print(base.id())\n    print(helper.two())\n}\n"),
        ("app/src/helper.tau", "pub fn two() -> i64 { 2 }\n"),
    ]);
    assert_eq!(run_both(&t.path("app")), "8\n7\n2\n");
}

#[test]
fn a_dependency_and_a_local_module_with_the_same_name_are_refused() {
    let mut files = fastmath_files();
    files.push(("app/tau.toml", "[package]\nname = \"app\"\n\n[dependencies]\nfastmath = { path = \"../fastmath\" }\n"));
    files.push(("app/src/main.tau", "use fastmath\nfn main() {}\n"));
    files.push(("app/src/fastmath.tau", "pub fn x() {}\n"));
    let t = Tmp::new(&files);
    let e = rejected(&t.path("app"));
    assert!(e.contains("ambiguous"), "{e}");
}

#[test]
fn two_different_packages_with_one_name_are_refused() {
    let t = Tmp::new(&[
        ("one/tau.toml", "[package]\nname = \"x\"\n"),
        ("one/src/lib.tau", "pub fn f() -> i64 { 1 }\n"),
        ("two/tau.toml", "[package]\nname = \"x\"\n"),
        ("two/src/lib.tau", "pub fn f() -> i64 { 2 }\n"),
        ("mid/tau.toml", "[package]\nname = \"mid\"\n\n[dependencies]\nx = { path = \"../two\" }\n"),
        ("mid/src/lib.tau", "use x\npub fn g() -> i64 { x.f() }\n"),
        ("app/tau.toml", "[package]\nname = \"app\"\n\n[dependencies]\nx = { path = \"../one\" }\nmid = { path = \"../mid\" }\n"),
        ("app/src/main.tau", "use x\nuse mid\nfn main() {\n    print(x.f() + mid.g())\n}\n"),
    ]);
    let e = rejected(&t.path("app"));
    assert!(e.contains("two different packages are both called `x`"), "{e}");
}

#[test]
fn package_problems_are_explained() {
    // a dependency folder that does not exist
    let t = Tmp::new(&[("tau.toml", "[package]\nname = \"a\"\n\n[dependencies]\nnope = { path = \"../nope\" }\n"), ("src/main.tau", "fn main() {}\n")]);
    let e = rejected(&t.0);
    assert!(e.contains("`nope`") && e.contains("cannot find"), "{e}");
    // a dependency without src/lib.tau
    let t = Tmp::new(&[
        ("dep/tau.toml", "[package]\nname = \"dep\"\n"),
        ("dep/src/main.tau", "fn main() {}\n"),
        ("app/tau.toml", "[package]\nname = \"app\"\n\n[dependencies]\ndep = { path = \"../dep\" }\n"),
        ("app/src/main.tau", "use dep\nfn main() {}\n"),
    ]);
    let e = rejected(&t.path("app"));
    assert!(e.contains("no src/lib.tau"), "{e}");
    // a project without src/main.tau
    let t = Tmp::new(&[("tau.toml", "[package]\nname = \"a\"\n")]);
    let e = rejected(&t.0);
    assert!(e.contains("neither src/main.tau"), "{e}");
    // a library can be checked, but there is nothing to run
    let t = Tmp::new(&[("tau.toml", "[package]\nname = \"a\"\n"), ("src/lib.tau", "pub fn f() {}\n")]);
    let (ok, out, _) = tau_in(&t.0, &["check"]);
    assert!(ok && out.contains("ok"), "{out}");
    let (ok, _, err) = tau_in(&t.0, &["run"]);
    assert!(!ok && err.contains("this package is a library"), "{err}");
}

#[test]
fn manifest_errors_are_explained() {
    let cases: &[(&str, &str)] = &[
        ("[dependencies]\n", "no [package]"),
        ("[package]\n", "needs `name"),
        ("[package]\nname = \"a b\"\n", "may only contain"),
        ("[package]\nname = \"a\"\n[dependencies]\nx = \"1.0\"\n", "registry"),
        ("[package]\nname = \"a\"\n[dependencies]\nx = { path = \"p\", git = \"g\" }\n", "not both"),
        ("[package]\nname = \"a\"\n[dependencies]\nx = {}\n", "needs `path"),
        ("[package]\nname = \"a\"\n[dependencies]\nx = { git = \"g\", tag = \"t\", branch = \"b\" }\n", "only one of"),
        ("[package]\nname = \"a\"\n[dependencies]\nx = { path = \"p\", flavour = \"q\" }\n", "unknown key"),
        ("[package]\nname = \"a\"\n[dependencies]\nstd = { path = \"p\" }\n", "reserved"),
        ("[package]\nname = \"a\"\n[dependencies]\nmy-dep = { path = \"p\" }\n", "cannot be used in `use`"),
        ("[package]\nname = \"a\"\nversion = \n", "line 3"),
    ];
    for (text, want) in cases {
        let t = Tmp::new(&[("tau.toml", text), ("src/main.tau", "fn main() {}\n")]);
        let e = rejected(&t.0);
        assert!(e.contains(want), "for {text:?} expected {want:?}, got:\n{e}");
    }
}

// ------------------------------------------------------------------ tau new, tau add

#[test]
fn new_creates_a_package_that_runs_and_a_library_that_tests() {
    let t = Tmp::new(&[]);
    let dir = project::new_package(&t.0, "hello", false).unwrap();
    assert_eq!(run_both(&dir), "hello, world\n");
    let lib = project::new_package(&t.0, "mylib", true).unwrap();
    let ws = project::load(&lib, LoadOptions { all_modules: true }).unwrap_or_else(|e| panic!("{}", e.render()));
    let r = tau::test_workspace(&ws, None, u64::MAX);
    assert!(r.diagnostics.is_empty() && r.results.len() == 1 && r.results[0].passed, "{:?}", r.results);
    assert!(project::new_package(&t.0, "hello", false).unwrap_err().contains("already exists"));
    assert!(project::new_package(&t.0, "bad name", false).is_err());
    assert!(project::new_package(&t.0, "", false).is_err());
}

#[test]
fn adding_a_dependency_edits_the_manifest_and_keeps_the_rest() {
    let base = "# my program\n[package]\nname = \"app\"\nversion = \"0.1.0\" # first\n\n[dependencies]\nold = { path = \"../old\" }\n\n[native]\nlink = [\"m\"]\n";
    let out = project::add_dependency_text(base, "fastmath", "{ path = \"../fastmath\" }").unwrap();
    assert_eq!(out, "# my program\n[package]\nname = \"app\"\nversion = \"0.1.0\" # first\n\n[dependencies]\nold = { path = \"../old\" }\nfastmath = { path = \"../fastmath\" }\n\n[native]\nlink = [\"m\"]\n");
    // no [dependencies] section yet
    let out = project::add_dependency_text("[package]\nname = \"app\"\n", "x", "{ path = \"p\" }").unwrap();
    assert_eq!(out, "[package]\nname = \"app\"\n\n[dependencies]\nx = { path = \"p\" }\n");
    // an empty section at the end of the file
    let out = project::add_dependency_text("[package]\nname = \"app\"\n\n[dependencies]\n", "x", "{ path = \"p\" }").unwrap();
    assert_eq!(out, "[package]\nname = \"app\"\n\n[dependencies]\nx = { path = \"p\" }\n");
    // twice
    let err = project::add_dependency_text(&out, "x", "{ path = \"q\" }").unwrap_err();
    assert!(err.contains("already"), "{err}");
}

// ------------------------------------------------------------------ the command line

fn tau_in(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_tau")).current_dir(dir).args(args).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

#[test]
fn the_command_line_works_on_projects() {
    let t = Tmp::new(&[]);
    let (ok, out, err) = tau_in(&t.0, &["new", "demo"]);
    assert!(ok, "{out}{err}");
    let demo = t.path("demo");
    // from inside the project, with no path
    let (ok, out, err) = tau_in(&demo, &["run"]);
    assert!(ok && out == "hello, world\n", "{out}{err}");
    let (ok, out, err) = tau_in(&demo.join("src"), &["run", "--interp"]);
    assert!(ok && out == "hello, world\n", "{out}{err}");
    let (ok, out, _) = tau_in(&demo, &["check"]);
    assert!(ok && out.trim() == "demo: ok", "{out}");
    // another package as a dependency
    let (ok, out, err) = tau_in(&t.0, &["new", "greet", "--lib"]);
    assert!(ok, "{out}{err}");
    let (ok, out, err) = tau_in(&demo, &["add", "greet", "--path", "../greet"]);
    assert!(ok, "{out}{err}");
    let manifest = std::fs::read_to_string(demo.join("tau.toml")).unwrap();
    assert!(manifest.contains("greet = { path = \"../greet\" }"), "{manifest}");
    std::fs::write(demo.join("src").join("main.tau"), "use greet\n\nfn main() {\n    print(greet.hello())\n}\n").unwrap();
    let (ok, out, err) = tau_in(&demo, &["run"]);
    assert!(ok && out == "hello from a library\n", "{out}{err}");
    // tests of the library, and of the whole package
    let (ok, out, err) = tau_in(&t.path("greet"), &["test"]);
    assert!(ok && out.contains("test hello_says_hello ... ok"), "{out}{err}");
    // a test filter, with and without a path
    let (ok, out, _) = tau_in(&t.path("greet"), &["test", "nomatch"]);
    assert!(ok && out.contains("running 0 tests"), "{out}");
    let (ok, out, _) = tau_in(&t.0, &["test", "greet", "hello"]);
    assert!(ok && out.contains("running 1 test"), "{out}");
    // an error shows the file and line
    std::fs::write(demo.join("src").join("main.tau"), "fn main() {\n    let x: i64 = \"a\"\n}\n").unwrap();
    let (ok, _, err) = tau_in(&demo, &["check"]);
    assert!(!ok && err.contains("main.tau:2:18"), "{err}");
    // outside any package
    let (ok, _, err) = tau_in(&t.0, &["run"]);
    assert!(!ok && err.contains("no tau.toml"), "{err}");
    let (ok, _, err) = tau_in(&t.0, &["run", "nothing.tau"]);
    assert!(!ok && err.contains("no such file"), "{err}");
}

#[test]
fn a_single_file_with_siblings_runs_from_the_command_line() {
    let t = Tmp::new(&[("main.tau", "use util\nfn main() {\n    print(util.f())\n}\n"), ("util.tau", "pub fn f() -> i64 { 3 }\n")]);
    let (ok, out, err) = tau_in(&t.0, &["run", "main.tau"]);
    assert!(ok && out == "3\n", "{out}{err}");
    let (ok, out, err) = tau_in(&t.0, &["run", "main.tau", "--interp"]);
    assert!(ok && out == "3\n", "{out}{err}");
    let (ok, out, _) = tau_in(&t.0, &["build", "main.tau", "--emit-c"]);
    assert!(ok && out.contains("tau_run_main"));
}

#[test]
fn programs_on_standard_input_still_work() {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut c = Command::new(env!("CARGO_BIN_EXE_tau")).args(["run", "-", "--interp"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    c.stdin.take().unwrap().write_all(b"fn main() { print(1 + 2) }\n").unwrap();
    let o = c.wait_with_output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout), "3\n");
}
