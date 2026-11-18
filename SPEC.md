# Tau: Language Spec (Phase 0 draft)

Status: **v1 design locked.** Nothing is implemented yet. Features marked **[post-v1]** are designed for but not built in v1 (see section 15).

Tau is a low-level, statically typed language focused on math and science. It is also meant to work as an ordinary systems/general-purpose language. Programs run locally across all cores by default. You can optionally name nodes to run on, and nodes can join private networks.

"Tau" is a working name.

---

## 1. Decisions

| Area | Decision |
|---|---|
| Blocks | Braces, no semicolons. A newline ends a statement. |
| Typing | Static, with inference inside function bodies. Function signatures are explicit. |
| Mutability | Immutable by default. `let mut` to allow changes. |
| Memory | Hybrid ownership. One owner by default, checked borrows, inferred lifetimes, opt-in `Shared<T>`, `unsafe` for raw access. No GC. |
| Errors | `Result<T, E>` plus the `?` operator. No exceptions. |
| Generics | Traits plus generics. Operator overloading through traits. |
| Compiler written in | Rust |
| Backend | LLVM first. Cranelift optional later. |
| Compile model | Ahead-of-time to native first. REPL/interpreter later. |
| Packages | Git URLs and local paths first. Central registry later. |
| Cluster trust | Private networks with a shared key. |
| Remote tasks | Pure, serialisable tasks only. |
| Code distribution | The same compiled binary on every node. WASM later. |
| Tensor layout | Column-major. |
| Floats | Strict IEEE 754 by default. Opt-in `fast_math`. |
| Licence | MIT or Apache-2.0, decided later. |
| Strings | Borrowed UTF-8 `str` plus owned `String`. |
| Number literal defaults | Unsuffixed `5` is `i64`. `5.0` is `f64`. |
| Panics | Abort the program. A panicking `spawn` task is reported as an error by `join()`. |
| Printing | Built-in `print(x)` for any type implementing `Display`. |
| Entry point | `fn main()`, optionally returning `Result<(), E>`. |
| Tests | `#[test]` functions, run by `tau test`. |
| Power vs xor | `^` is power (right-associative). `^^` is bitwise xor. |
| Call-site `mut` | Required: `scale(mut data, 2.0)`. |
| File extension / CLI | `.tau` / `tau` |

---

## 2. Lexical basics

- Source is UTF-8. File extension: `.tau`.
- Line comment: `// ...`. Block comment: `/* ... */` (nestable).
- Statements end at a newline. A line continues if it ends with a binary operator, `,`, or an open bracket, or if the next line starts with `.` (method chain).
- Identifiers: letters, digits and `_`, not starting with a digit. Unicode letters allowed, so `θ` and `Δt` are valid names.
- Integer literals: `42`, `1_000_000`, `0xFF`, `0b1010`. Float literals: `3.14`, `1e-9`, `2.5e3`.
- An unsuffixed integer literal is `i64` unless inference says otherwise, and a float literal is `f64`. Suffixes force a type: `5u8`, `2.0f32`.
- Attributes: `#[name]` or `#[name(args)]` on the line before an item, e.g. `#[test]`.
- Strings: `"text"` with `{expr}` interpolation, so `"x = {x}"`. Use `\{` for a literal brace.
- Keywords (small set): `fn let mut const struct enum trait impl use pub if else match for while loop break continue return own par spawn on unsafe type as in true false where cluster`
  (`where` guards `for` loops; `cluster` is reserved for the post-v1 cluster declaration. `self` and `Self` are ordinary identifiers with special meaning in `impl`/`trait`.)

### Grammar notes (settled while writing the stage-1 parser)
- **Newlines.** A line break ends a statement unless the line ends with a binary operator, `,`, `=`, `->` or `=>`; the next line starts with `.` (a method chain); or the break is inside `( )` or `[ ]` (but not `{ }`). `else` may start the line after the closing `}` of an `if`.
- **Semicolons** are a hard error with a hint, except inside `[T; N]` and `[value; count]`.
- **Operator precedence**, loosest to tightest: assignment, `..`, `||`, `&&`, comparisons, `|`, `^^`, `&`, `<< >>`, `+ -`, `* / % @`, `as`, unary `- ! ~ mut`, `^`, postfix. Power binds tighter than unary minus (`-x^2` is `-(x^2)`) and is right-associative.
- **Generics in expressions** have no turbofish: `Map<String, usize>.new()` and `alloc<u8>(n)` are read as generic applications when the `<...>` parses as types and the closing `>` is directly followed by `.` or `(`. Otherwise `<` is a comparison. This means `f(a < b, c > (d))` is misread as a generic call; write `f(a < b, (c > d))`.
- **`>>`** is a shift only when the two `>` are adjacent, so `Vec<Vec<f64>>` works.
- **Parameter modes.** `own` goes before the name (`own v: Vec<f64>`), `mut` goes on the type (`v: mut [f64]`). `self` takes `mut self` / `own self`.
- **Struct construction** is positional, like a call: `Vec3(1.0, 2.0, 3.0)`. There is no `Name { field: value }` form, so `if x { ... }` is never ambiguous.
- **Unit literals** (`9.8 m/s`) do not parse in v1. The form is reserved.
- **Builtin constructors** such as `vec[1, 2, 3]` parse as indexing with several indices; name resolution (stage 2) will treat `vec` specially.
- **Tuple fields** are `t.0`, `t.0.1`.
- **Numeric literals**: `1_000`, `0xFF`, `0b1010`, `0o17`, `1e-9`, suffixes `u8..u128`, `i8..i128`, `usize`, `isize`, `f32`, `f64`, and `4i` for an imaginary literal.

---

## 3. Types

### Primitives
- Integers: `i8 i16 i32 i64 i128 isize`, `u8 u16 u32 u64 u128 usize`
- Floats: `f32 f64`
- Complex: `c64 c128` (pairs of `f32`/`f64`), written like `3 + 4i`
- `bool`, `char` (a Unicode scalar), `str` (borrowed UTF-8 string slice), `String` (owned, growable UTF-8 string)
- String literals have type `str`. `s.to_string()` or `String.from(s)` makes an owned copy.

Integer overflow panics in debug builds and wraps in release builds. Explicit `wrapping_add`, `checked_add` and `saturating_add` exist for when you need a specific behaviour.

There are no implicit numeric conversions. Use `x as f64`.

### Compound types
- Tuples: `(i32, f64)`
- Fixed arrays: `[f64; 3]` (size known at compile time)
- Slices: `[f64]` (a borrowed view of contiguous data)
- Growable vectors: `Vec<T>`
- Structs, enums, and `Option<T>` / `Result<T, E>` (enums in the standard library)

### Tensors
```
let a: Tensor<f64, [3, 3]> = tensor.zeros()
let b = Tensor<f64, [_, _]>.random(100, 50)   // dynamic dimensions
```
- One `Tensor` type. Each dimension is a fixed number or `_` (dynamic).
- Column-major storage. Dense only in v1.
- `a @ b` is matrix multiplication (BLAS-backed) and `a * b`, `a + b` and so on are elementwise.
- **v1:** shape mismatches are runtime panics with a clear message. **[post-v1]:** symbolic dimensions (`N`, `M`) and compile-time shape checking.

### Units [post-v1]
The literal form `9.8 m/s` is reserved in v1 so units can be added later without breaking code.
```
let d = 9.8 m/s * 3 s        // type is Quantity<f64, m>
let bad = d + 3 s            // compile error: m + s
```
Units are tracked in the type and cost nothing at runtime. Users can define new units.

---

## 4. Variables and functions

```
let x = 5                // immutable, inferred i32
let mut total = 0.0      // mutable
const G: f64 = 6.674e-11 // compile-time constant

fn hypot(a: f64, b: f64) -> f64 {
    sqrt(a^2 + b^2)      // last expression is the return value
}
```
- `^` is exponentiation, right-associative, so `2^3^2 == 2^9`. Bitwise xor is `xor(a, b)` or `a ^^ b`.
- Ranges: `0..n` (exclusive), `0..=n` (inclusive).
- Closures: `|x| x * 2` and `|a, b| { a + b }`.

---

## 5. Control flow

```
if x > 0 { ... } else if x == 0 { ... } else { ... }
let sign = if x >= 0 { 1 } else { -1 }       // if is an expression

for i in 0..10 { ... }
for x in values { ... }
while cond { ... }
loop { if done { break } }

match shape {
    Circle(r) => PI * r^2
    Rect(w, h) => w * h
    _ => 0.0
}
```

---

## 6. Structs, enums, traits

```
struct Vec3 { x: f64, y: f64, z: f64 }

impl Vec3 {
    fn dot(self, o: Vec3) -> f64 { self.x*o.x + self.y*o.y + self.z*o.z }
}

enum Shape {
    Circle(f64)
    Rect(f64, f64)
}

trait Number {
    fn zero() -> Self
    fn add(self, o: Self) -> Self
}

fn sum<T: Number>(xs: [T]) -> T {
    let mut acc = T.zero()
    for x in xs { acc = acc.add(x) }
    acc
}
```

### Operator overloading
Operators are traits, so user types get math syntax:
```
impl Add for Vec3 {
    fn add(self, o: Vec3) -> Vec3 { Vec3(self.x+o.x, self.y+o.y, self.z+o.z) }
}
let c = a + b
```
Traits: `Add Sub Mul Div Neg Pow MatMul Index Eq Ord`. The compiler rewrites `a + b` to `Add.add(a, b)`.

---

## 7. Memory model (hybrid ownership)

Rules:
1. Every value has exactly one owner. When the owner goes out of scope the value is freed.
2. `let b = a` **moves** a non-`Copy` value. `a` can't be used afterwards. Primitives and small plain structs marked `Copy` are copied instead.
3. Function parameters **borrow read-only** by default. `mut T` borrows mutably. `own T` takes ownership.
4. At any time, a value has either many read-only borrows or one mutable borrow, never both.
5. A borrow can't outlive its owner. Lifetimes are inferred, and only ambiguous cases need an annotation.

```
fn scale(v: mut [f64], k: f64) {         // mutable borrow
    for x in mut v { x *= k }
}

fn total(v: [f64]) -> f64 { sum(v) }      // read-only borrow

fn consume(own v: Vec<f64>) { ... }       // takes ownership

fn main() {
    let mut data = vec[1.0, 2.0, 3.0]
    scale(mut data, 2.0)                  // call site marks the mutable borrow
    let t = total(data)
    consume(data)
    // print(data[0])                     // error: data was moved
}
```

Escape hatches:
- `Shared<T>`: reference-counted shared ownership, atomic when sent across threads. For mutation, `Shared<Mutex<T>>`.
- `unsafe { ... }`: raw pointers, manual allocation (`alloc` / `free`), unchecked indexing, calling C.

No garbage collector. No hidden allocations: anything that allocates says so (`Vec`, `String`, `Shared`, `alloc`).

---

## 8. Errors

```
fn read_config(path: str) -> Result<Config, IoError> {
    let text = fs.read(path)?          // returns early on error
    parse(text)
}

match read_config("a.cfg") {
    Ok(c) => use_config(c)
    Err(e) => print("failed: {e}")
}
```
A `panic("msg")` stops the program for unrecoverable bugs (index out of range, failed assertion). Panics abort: they print the message and location, then exit with a non-zero code. If a `spawn`ed task panics, `h.join()` returns `Err(TaskPanicked)` to the caller instead of aborting.

`main` may return `Result<(), E>`. An `Err` prints the error and exits with code 1.

## 8b. Tests
```
#[test]
fn adds() {
    assert(1 + 1 == 2)
    assert_eq(hypot(3.0, 4.0), 5.0)
}
```
`tau test` compiles and runs every `#[test]` function and reports pass/fail per test.

---

## 9. Modules and packages

```
use math.linalg
use math.fft.{fft, ifft}
use mypkg.tools as t

pub fn visible() { ... }        // exported
fn hidden() { ... }             // private to the module
```
- One file is one module. A folder with `mod.tau` is a module with submodules.
- Layers: `core` (no allocation), `std` (files, collections, threads, net), `math` (tensors, linear algebra, FFT, ODEs, stats, units).

### Manifest `tau.toml`
```
[package]
name = "orbits"
version = "0.1.0"

[dependencies]
fastmath = { git = "https://example.com/fastmath", tag = "v1.2" }
localutil = { path = "../localutil" }

[native]
link = ["openblas"]
```
- `tau new`, `tau build`, `tau run`, `tau test`, `tau add <dep>`.
- A `tau.lock` file pins exact versions for reproducible builds.
- Users can ship native C/Fortran code through `[native]`.

---

## 10. Parallelism (local)

Parallelism is explicit and checked by the ownership rules. Data crossing a task boundary must be moved, or borrowed read-only, so data races are rejected at compile time.

```
// parallel loop: each iteration gets a disjoint slice or index
par for i in 0..n {
    out[i] = f(inp[i])
}

// parallel reduction
let s = par sum(i in 0..n) a[i] * b[i]

// tasks
let h = spawn { heavy(data) }     // data moved or read-borrowed
let r = h.join()
```
Write rules for `par for`:
- Writing `out[i]`, where `i` is the loop variable itself, is allowed (each iteration owns a distinct element).
- Any other write to data from outside the loop is a compile error.
- For harder patterns use `par for chunk in par_chunks(mut out, size) { ... }`, which hands each iteration a disjoint mutable slice.

Backed by a work-stealing thread pool that uses all cores by default (`--threads N` to change).

---

## 11. Shared compute (nodes and clusters) [post-v1, planned for v2]

The syntax below is reserved in v1 but not implemented. Default is local. Nothing below is required for single-machine use.

```
cluster lab = nodes ["10.0.0.5:7000", "10.0.0.6:7000"]

let r = on lab {
    simulate(grid)          // runs on nodes in `lab`
}

on local { ... }            // explicit local (the default)
```
- A task given to `on` must be **pure and serialisable**: its captured inputs are plain data (no raw pointers, no open files), and its result is plain data.
- `par for` inside `on lab` spreads iterations across the cluster's workers. The runtime partitions tensors into chunks.
- Local threads and remote nodes both implement a `Worker` trait, so the scheduler treats them the same. Users can implement `Worker` for custom targets such as a GPU or a batch queue.

### Nodes
```
tau node start --listen 0.0.0.0:7000 --key <shared-key>
tau node join <addr> --key <shared-key>
tau run prog.tau --nodes lab.toml
```
- Private networks only for v1: all nodes share a key, and connections are authenticated and encrypted.
- Every node runs the same compiled binary (checked by hash on join).
- If a node disappears mid-task, its tasks are re-run elsewhere, which is safe because they're pure.

---

## 12. Example programs

### 12.1 Hello
```
fn main() {
    print("hello, world")
}
```

### 12.2 FizzBuzz (general-purpose)
```
fn main() {
    for i in 1..=100 {
        match (i % 3, i % 5) {
            (0, 0) => print("FizzBuzz")
            (0, _) => print("Fizz")
            (_, 0) => print("Buzz")
            _ => print("{i}")
        }
    }
}
```

### 12.3 Word counter (std, errors, maps)
```
use std.fs
use std.map.Map

fn main() -> Result<(), IoError> {
    let text = fs.read("book.txt")?
    let mut counts = Map<String, usize>.new()
    for w in text.split_whitespace() {
        counts[w.lowercase()] += 1
    }
    for (w, n) in counts.top(10) {
        print("{w}: {n}")
    }
}
```

### 12.4 Numerical integration (trapezoid rule, parallel)
```
fn integrate(f: fn(f64) -> f64, a: f64, b: f64, n: usize) -> f64 {
    let h = (b - a) / n as f64
    let s = par sum(i in 1..n) f(a + i as f64 * h)
    h * (s + 0.5 * (f(a) + f(b)))
}

fn main() {
    print(integrate(|x| sin(x), 0.0, PI, 10_000_000))   // about 2.0
}
```

### 12.5 Matrix operations
```
use math.linalg

fn main() {
    let a = Tensor<f64, [_, _]>.random(1000, 1000)
    let b = Tensor<f64, [_, _]>.random(1000, 1000)
    let c = a @ b
    let (vals, vecs) = linalg.eig(c)
    print("trace = {trace(c)}")
}
```

### 12.6 Units [post-v1]
```
fn main() {
    let v0 = 20 m/s
    let g = 9.8 m/s^2
    let t = 2 s
    let h = v0 * t - 0.5 * g * t^2     // type: Quantity<f64, m>
    print("height = {h}")
}
```

### 12.7 N-body step (structs, mutable borrow)
```
struct Body { pos: Vec3, vel: Vec3, mass: f64 }

fn step(bodies: mut [Body], dt: f64) {
    for i in 0..bodies.len {
        let mut acc = Vec3(0.0, 0.0, 0.0)
        for j in 0..bodies.len where j != i {
            let d = bodies[j].pos - bodies[i].pos
            acc += d * (G * bodies[j].mass / d.norm()^3)
        }
        bodies[i].vel += acc * dt
    }
    for b in mut bodies { b.pos += b.vel * dt }
}
```

### 12.8 A user-defined number type and package use
```
// quaternion.tau, published as a package
pub struct Quat { w: f64, x: f64, y: f64, z: f64 }

impl Mul for Quat {
    fn mul(self, o: Quat) -> Quat { /* Hamilton product */ }
}

// elsewhere
use quaternion.Quat
let r = q1 * q2
```

### 12.9 Running on a cluster [post-v1]
```
cluster lab = nodes ["10.0.0.5:7000", "10.0.0.6:7000"]

fn main() {
    let samples = 400_000_000
    let hits = on lab {
        par sum(i in 0..samples) monte_carlo_point(i)
    }
    print("pi is about {4.0 * hits as f64 / samples as f64}")
}
```

### 12.10 Low-level / unsafe
```
fn fill(n: usize, v: u8) -> Vec<u8> {
    unsafe {
        let p = alloc<u8>(n)
        for i in 0..n { p[i] = v }
        Vec.from_raw(p, n)
    }
}
```

---

## 13. Compiler architecture

```
source -> lexer -> parser -> AST -> name resolution
       -> type, shape, unit and borrow checking -> typed IR
       -> optimiser (inlining, fusion, tiling) -> LLVM IR -> native binary
```
- Lexer handles newline-as-terminator and continuation rules.
- IR is a typed, SSA-style graph that keeps parallel regions explicit so the scheduler can split them.
- Runtime library: thread pool, tensor memory, node networking (linked in only if used).
- Errors must be good. Every compile error names the line, shows the code, and says what to change.

---

## 14. Resolved questions
1. `^` is power, `^^` is xor.
2. Call-site `mut` is required.
3. Unit syntax is postponed with units. The `9.8 m/s` literal form is reserved.
4. `par for` allows `out[i]` writes with the loop index. `par_chunks` covers harder cases.
5. Closure capture across `on` is postponed with clusters. For `spawn`, captures are moved or read-borrowed.
6. Extension `.tau`, CLI `tau`.
7. One `Tensor` type, with `_` for dynamic dimensions.

Still open, for v2: the exact definition of "pure" for cluster tasks.

---

## 14b. Type-checking rules (settled while writing the stage-2 checker)

**Inference.** Types are inferred inside function bodies by unification; signatures are explicit.
An unsuffixed integer literal has a hidden "integer-literal" type that may become any integer,
float or complex type, and a float literal any float or complex type, so `2 * x` works for
`x: f64` and `let y: f64 = 1` is allowed. Variables themselves never convert: `let a: i64 = 1`
then `let b: f64 = a` is an error (write `a as f64`). A literal nothing constrains defaults to
`i64` / `f64`. An unconstrained *non*-literal type is an error ("cannot infer a type here").

**Coercions** (the only implicit conversions): `Never` to any type; `[T; N]` and `Vec<T>` to `[T]`;
`String` to `str`. They apply to call arguments, `let` with a type, assignment and returns.

**A function that returns unit may end in an expression of any type.** There are no semicolons to
discard a value with, so `fn f() { v.pop() }` is allowed. (A function with any other return type
must produce exactly that type.)

**Modes and mutability.**
- Parameters are read-only borrows by default; `v: mut T` is a mutable borrow; `own v: T` takes
  ownership. Inside the function, `mut` and `own` parameters may be modified; plain ones may not.
- The call site must write `mut` exactly for `mut` parameters: `scale(mut data, 2.0)`. A `mut` that
  is missing, or on a parameter that doesn't take one, is an error. `mut` is only legal on call
  arguments and on a `for` iterable.
- Assigning to a place (`x = ..`, `p.f = ..`, `a[i] = ..`, `x += ..`) or borrowing it `mut` requires
  the root variable to be `let mut` or a `mut`/`own` parameter, and a `mut self` method requires
  a mutable receiver. `for x in mut v` binds `x` mutably; the items of `par_chunks(mut v, n)`
  are mutable slices. Writing through a raw pointer (`p[i] = ..`) does not need `let mut p`.
- Ownership and borrow *conflicts* (use after move, aliasing) are stage 3, not checked yet.

**Operators.**
- Built-in numbers, `Num`-bounded type parameters, and tensors use the operators directly. Tensor
  `+ - * /` are elementwise (also with a scalar); `a @ b` is a matrix or vector product whose result
  shape is computed from the operands. Fixed dimensions that cannot line up are an error today;
  everything else is checked at run time in v1.
- On any other type the operator is an overloaded trait call: `+ - * / % ^ @ & | ^^ << >>` use
  `Add Sub Mul Div Rem Pow MatMul BitAnd BitOr BitXor Shl Shr`; unary `-` uses `Neg`; `[]` on
  non-array types uses `Index`; `==` `!=` use `Eq` (`eq`); `< <= > >=` use `Ord` (`lt`). An operator
  trait impl may use any signature with the right method name and arity, and the impl is chosen by
  the type of the right-hand operand, so `impl Mul for V { fn mul(self, k: f64) -> V }` and
  `impl Mul for V { fn mul(self, o: V) -> f64 }` can coexist.

**Traits.** `Num Int Float Copy Clone Eq Ord Display` hold for the built-in types by rule, and for any
other type by an explicit `impl`. A generic function's bounds are checked at every call. Methods
without a body in a trait must be implemented; methods with a body are defaults. Impls on built-in
types (`impl Double for i64`) are allowed.

**Patterns.** A bare name in a pattern that is an enum variant with no fields (`None`, `Empty`) is
that variant; any other bare name binds. Enum variants live in one namespace with functions, so two
enums in one file cannot both have a variant `Empty`. `match` on an enum must name every variant (or
have a catch-all arm), on `bool` both values; on anything else it needs a catch-all `_` arm.

**Unsafe.** Calling an `unsafe fn` and indexing a raw pointer (`Ptr<T>`) both require an `unsafe` block.

**The built-in library** is declared in Tau itself (`src/prelude/*.tau`): `prelude.tau` is visible
everywhere, and `std.fs`, `std.io`, `std.map`, `math.linalg`, `math.fft` are reached with `use`.
Functions declared without a body are provided by the runtime. User definitions shadow the prelude.

**Not checked yet** (deliberately deferred): `pub`/privacy across modules (there is one file today);
duplicate or overlapping impls (the first match wins); associated types; closure capture analysis;
`struct`/`enum`/`impl` declared inside a function; compile-time checking of symbolic tensor
dimensions; ownership and borrowing (stage 3).

---

## 15. v1 scope and build plan

### In v1
- Core language: functions, `let`/`mut`/`const`, structs, enums, `match`, closures, traits, generics, operator overloading, modules.
- Primitives, `str`/`String`, string interpolation.
- Hybrid ownership: moves, borrows, `own`, `Shared<T>`, `unsafe`.
- `Result`, `?`, panics, `#[test]`.
- Collections: `Vec`, `Map`, slices.
- Std basics: files, strings, printing, math functions.
- Native compilation through LLVM.
- Local parallelism: `par for`, `par sum`, `par_chunks`, `spawn`, work-stealing thread pool.
- Dense column-major `Tensor` with `@`, elementwise ops, BLAS binding, runtime shape checks.
- Packages: `tau.toml`, `tau.lock`, git and path dependencies, `tau new/build/run/test/add`.
- C interop.
- Clear error messages and a formatter (`tau fmt`).

### Not in v1
- Clusters, nodes, `on` (v2).
- Units of measure and compile-time shape checking (v1.5 or v2).
- GPU, central package registry, macros, REPL/notebook, language server.

### Build stages
Each stage ends with tests and real example programs running.

| Stage | Deliverable |
|---|---|
| 1 | Lexer, parser and AST, tested against the spec examples. |
| 2 | Name resolution and type checker, with inference and traits. |
| 3 | Borrow checker. |
| 4 | LLVM code generation and a minimal runtime. |
| 5 | Std library and tensors. |
| 6 | Thread pool and `par`. |
| 7 | Package manager, `tau test`, `tau fmt`. |

### After v1
| Version | Deliverable |
|---|---|
| v1.5 | Units of measure and compile-time shape checking. |
| v2 | Nodes, `on`, serialisation, networking, sandboxing. |
| Later | GPU, registry, macros, REPL/notebook, language server. |
