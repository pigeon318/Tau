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
| Licence | MIT OR Apache-2.0 (your choice of either), see `LICENSE-MIT` and `LICENSE-APACHE`. |
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

### Vector and slice methods

```
let mut v = vec[5, 3, 9, 1]
v.sort()                  // [1, 3, 5, 9]      (needs the elements to be ordered)
v.sort_by(|a, b| a.len < b.len)    // sort with your own "goes before": `less(a, b)`
v.reverse()   v.swap(i, j)   v.fill(0)
v.insert(i, x)   v.remove(i)   v.swap_remove(i)   v.truncate(n)   v.clear()   v.retain(|x| x > 3)   v.extend(other)
v.contains(x)   v.position_of(x)   v.first()   v.last()   v.get(i)   v.is_sorted()   v.is_empty()
```

- **On a slice, they work on the original**: `v[1..4].sort()` sorts that part of `v` in place (`sort`, `sort_by`, `reverse`, `swap`, `fill` take a `mut` slice, so they also work on arrays and on a `mut [T]` parameter). `insert`, `remove`, `swap_remove`, `truncate`, `clear`, `retain` and `extend` change the length, so they need a `Vec`.
- `sort` and `sort_by` are **heap sort**: O(n log n) and not stable (equal elements may change their relative order). Floats sort with `<`, so NaN has no defined place. Elements are moved with `swap`, never copied, so sorting strings or vectors is cheap and needs no `Clone`.
- `insert(i, x)` takes `i` up to and including the length; `remove(i)` and `swap_remove(i)` need `i` less than the length (otherwise a panic that says so). `swap(i, j)` panics with `index out of bounds` like indexing. `swap_remove` is O(1) but moves the last element into the gap. `retain` keeps the order.
- `first`, `last`, `get` and `extend` copy elements with `clone`, so the elements must be `Clone`; `contains` and `position_of` need `Eq`; `sort` and `is_sorted` need `Ord`. A method whose bound is not met says which one: "`sort` needs `P: Ord`".

### Tensors
```
let a: Tensor<f64, [3, 3]> = tensor.zeros()
let b = Tensor<f64, [_, _]>.random(100, 50)   // dynamic dimensions
```
- One `Tensor` type. Each dimension is a fixed number or `_` (dynamic).
- Column-major storage. Dense only in v1.
- `a @ b` is matrix multiplication (BLAS-backed) and `a * b`, `a + b` and so on are elementwise.
Building and summarising tensors (all deterministic, and identical on both backends):

```
let a = Tensor<f64, [_]>.from([1.0, 2.0, 3.0, 4.0])           // from an array, slice or Vec
let m = Tensor<f64, [_, _]>.from_rows(vec[vec[1.0, 2.0], vec[3.0, 4.0]])
let i = Tensor<f64, [_, _]>.eye(3)                              // identity
let f = Tensor<f64, [_, _, _]>.full(7.5, 2, 3, 4)               // every element 7.5
let x = Tensor<f64, [_]>.linspace(0.0, 1.0, 11)                 // n evenly spaced points, ends included
let n = Tensor<f64, [_]>.arange(5)                              // 0.0, 1.0, 2.0, 3.0, 4.0

a.sum()  a.mean()  a.min()  a.max()                             // reductions (mean: floats and complex; min/max: floats and integers)
a.shape()          // Vec<usize>, e.g. [2, 3]
a.to_vec()         // Vec of the elements, in storage order
a.map(|x| x * x)   // a new tensor
m.reshape(4)       // a copy with another shape (the same number of elements)
m.transpose()      // rank 2
```

- Elements are stored **column-major** (the first index varies fastest), so `to_vec` and `reshape` follow that order: reshaping `[[1, 2, 3], [4, 5, 6]]` to `(3, 2)` gives `[[1, 5], [4, 3], [2, 6]]`, as in Fortran and Julia.
- `sum` adds in storage order starting from `0.0`, and `mean` is that sum divided by the number of elements (NaN for none). `min` and `max` panic on an empty tensor. `linspace(a, b, n)` is `a + (b - a) * i / (n - 1)`, with exactly `a` and `b` at the ends. For `f32` tensors every result is rounded to single precision.
Tensors of integers and of `bool`:

```
let a = Tensor<i64, [_]>.from([3, 1, 4, 1, 5])      // any integer type of up to 64 bits: i8 .. i64, u8 .. u64, isize, usize
let n = Tensor<u8, [_]>.arange(5)                    // 0, 1, 2, 3, 4  (arange also works for integers)
let z = Tensor<i32, [_, _]>.zeros(2, 3)              // zeros, ones, full, eye, from, from_rows, reshape, transpose, slices: as for floats
let m = a > 2                                        // a tensor of bool: [true, false, true, false, true]
a == b   a != b   a < 2   2 <= a                     // every comparison is element by element, with a tensor or a number
m & n   m | n   m ^^ n   !m                          // combining masks
m.count()   m.any()   m.all()                        // how many are true; whether any / all are
a.select(m)                                          // the elements where the mask is true, as a rank-1 tensor
a.to_f64()   a.to_i64()                              // another element type (floats are cut towards zero and saturate)
a + b   a - b   a * 2   a / 2   a % 3   -a   a & 3   a | b   a ^^ 1   a @ b   a.sum()   a.min()   a.max()
```

- An integer tensor keeps its integer type, and **every result is checked against its range**, exactly like the same operation on single integers: `Tensor<u8, [_]>.full(250, 3) + 10` panics with `integer overflow: 260 does not fit in u8`, `t / 0` with `division by zero`. The same holds for `sum`, `-a`, `a @ b`, `arange` and `to_i64`. `mean` needs floats (write `a.to_f64().mean()`); `min` and `max` work on integers and floats and panic on an empty tensor.
- **Comparisons give a tensor, not a `bool`**, so `if a == b { .. }` is a type error: write `if (a == b).all() { .. }`. Tensors of different shapes cannot be compared (a panic with both shapes), and complex tensors only have `==` and `!=`. `bool` tensors have no `+` or `<`; use `&`, `|`, `^^` and `!`, or convert with `to_i64()` or `to_f64()`.
- `select(mask)` needs a mask of exactly the tensor's shape, and works for every element type. The result is in storage order (column-major).
- Elements of `i128`, `u128`, `char`, strings and other types are not allowed in tensors. `random` makes floats only, and `linspace` too.
- **v1:** an integer or `bool` element takes 8 bytes in native code (the same storage as `f64`), so a `Tensor<u8, ..>` is not smaller than a `Tensor<f64, ..>`; packed storage is a later optimisation. `a += b` on whole tensors means `a = a + b` (a new tensor replaces the old one).

Parts of a tensor:

```
m[1, 2]            // one element
m[1, ..]           // row 1, a tensor of rank 1;      m[.., 2]  column 2
m[0..2, 1..]       // a block (rank 2);   ranges: a..b  a..=b  ..b  ..=b  a..  ..
m[1, 0..3] = 0.0              // every element of the part
m[.., 0] = Tensor<f64, [_]>.arange(4)       // a tensor of that shape (`a[..] = b` copies b into a)
```

- A part is a **copy**, not a view: changing the copy does not change the tensor, and the copy is yours (a `let mut row = m[0, ..]`). Each position (`i`) removes its axis and each range keeps it, so `m[1, 0..3]` has rank 1 and `m[0..2, 1..3]` rank 2. Writing to a part needs `let mut`.
- Assigning copies the elements: the value on the right (a tensor of exactly the shape of the part, or one number for every element) is only read, so it can be used afterwards. `+=` and the other operators are not available for a part; write `t[..] = t[..] + x`.
- Out-of-range positions and ranges, and a shape that does not fit, are panics with the axis and the sizes: `slice 1..9 is out of range for axis 0 of length 4`. A range end beyond the length is an error (it is not cut), and `a..` ends at the length.
- Views that alias the tensor (so that writing to a part changes the whole without copying) are planned for later.
- The constructors need the rank to match (`from`, `linspace`, `arange` make rank 1; `from_rows` and `eye` rank 2; `full` any rank).
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

### Boxes and recursive types

`Box<T>` owns one `T` on the heap. It is what lets a type contain itself (a type cannot hold a value of its own type directly, because it would have to be infinitely large; it can hold a `Box` of it, or a `Vec` of it):

```
enum List {
    Nil
    Cons(i64, Box<List>)
}

fn sum(l: List) -> i64 {
    match l {
        Nil => 0
        Cons(x, rest) => x + sum(rest)       // rest: Box<List>, used where a List is borrowed
    }
}

let l = Cons(1, Box.new(Cons(2, Box.new(Nil))))
```

- `Box.new(x)` moves `x` into a box; `b.take()` moves it out again (`b` is gone).
- A box is looked through wherever a borrow of the value it holds is wanted: a field access (`b.value`), a method call (`b.push(1)`), a `match` (when an arm has a pattern), and an argument passed to a *borrowed* parameter of the held type (`sum(rest)`). It is not looked through where a value is moved: `let x: T = b` and an `own` parameter of type `T` are type errors; use `b.take()`.
- A box can be compared (`==`, `<`) and printed when what it holds can.
- **Taking an owned value apart.** `match t { Node(l, x, r) => ... }` on a variable binds *views* of its parts, which cannot be moved. To move the parts out, mark the scrutinee: `match own t { Node(l, x, r) => Node(Box.new(f(l.take())), x, r) }`. `own e` is the value of `e` moved out of its place (a copy for a `Copy` type), so `t` cannot be used afterwards, and a borrowed parameter cannot be moved from. This is how a tree is rebuilt by value (see `examples/checked/trees.tau`).
- Mutating through a `match` binding is not possible yet (the bindings are copies of the parts); rebuild the value as above, or keep the data in a `Vec`.

Escape hatches:
- `Shared<T>`: reference-counted shared ownership, atomic when sent across threads; read-only. For mutation, `Shared<Mutex<T>>` (see "Shared values and mutexes").
- `unsafe { ... }`: raw pointers, manual allocation (`alloc` / `free`), unchecked indexing, calling C.

No garbage collector. No hidden allocations: anything that allocates says so (`Vec`, `String`, `Shared`, `alloc`).

---

### Iterators

An iterator is a value with a `next` method: each call gives `Some(item)`, then `None` when there are no more. The trait is in the prelude:

```
trait Iterator<T> {
    fn next(mut self) -> Option<T>
    // ... and the methods below, which have bodies
}

struct Countdown { n: i64 }
impl Iterator<i64> for Countdown {
    fn next(mut self) -> Option<i64> {
        if self.n == 0 { return None }
        self.n -= 1
        Some(self.n + 1)
    }
}
```

- **`for` loops.** `for x in it` works for any value that has an `impl Iterator<T>` (the items are `T`). The loop uses the iterator up; `for x in mut it` only borrows it, so a `break` leaves it where it stopped. Ranges, arrays, vectors and slices are looped over directly, as before.
- **Where iterators come from.** `v.iter()` for an array, a vector or a slice, and `(a..b).iter()` for a range of integers. `v.iter()` gives the items **by value** (a copy for numbers, a `clone` for strings and other owned things), so the items never keep `v` borrowed; the iterator itself holds `v` borrowed for as long as it is used. `for x in v` is still the way to look at the items without copying them.
- **Adapters** turn an iterator into another one and do no work by themselves: `map(f)`, `filter(f)`, `enumerate()` (items are `(usize, T)`), `zip(other)` (items are pairs; ends with the shorter), `take(n)`, `skip(n)`, `step_by(n)` (`n` must not be 0), `chain(other)`, `take_while(f)`, `skip_while(f)`.
- **Consumers** use the iterator up: `collect()` (a `Vec<T>`), `count()`, `sum()` and `product()` (numbers; an empty iterator gives 0 and 1), `min()` and `max()` (an `Option`; the items must be ordered; of equal items the first is kept for `min` and the last for `max`), `fold(init, f)`, `for_each(f)`, `any(f)`, `all(f)`, `find(f)`, `position(f)`, `last()`, `nth(n)`.
- **Laziness.** Each item goes through the whole chain before the next one is asked for, so `v.iter().filter(..).map(..).sum()` makes no vectors in between, and the compiler turns it into a plain loop (as fast as writing the loop by hand). The closures given to an adapter see their argument by borrow, like every closure; a closure that must hand its argument on writes `x.clone()` or builds a new value.
- **Iterators are ordinary values.** The adapters are structs (`MapIter`, `FilterIter`, ...) with `impl Iterator`, so a function can take `own it: I` with `I: Iterator<T>`: a bound like that tells the compiler what `T` is, as long as exactly one `impl` fits `I`. An iterator that holds closures or slices is a view: it cannot outlive what it was made from.
- **Not yet:** `rev()` and other double-ended steps, `peekable`, `flat_map`, `windows`/`chunks`, and `(a..b).map(..)` without `.iter()` (write `(a..b).iter().map(..)`).

Two helpers for generic code: `zero<T: Num>() -> T` and `one<T: Num>() -> T` (an integer literal is not a `T`). `x.clone()` works on any type (it copies vectors, strings, boxes and maps too), and `Option` and `Result` print as `Some(3)`, `None`, `Ok(..)` and `Err(..)` when their contents can be printed. A range is a value: `let r = 2..5` has `r.start` and `r.end` (the end is always the exclusive one, so `1..=4` has `end` 5).

### Shared values and mutexes

```
let cfg = Shared.new(Config(...))     // one value with several owners
let other = cfg.clone()               // another handle to the same value (the value is not copied)
print(cfg.name)  area(cfg, 2.0)  cfg[0]  match cfg { ... }     // read through any handle, like a Box

let counter = Shared.new(Mutex.new(0))
let c = counter.clone()
let t = spawn { for _ in 0..1000 { c.update(|n| n + 1) } }      // `c` is moved into the task
let _ = t.join()
print(counter.get())                  // 1000
```

- **`Shared<T>`** is reference counted. The count is atomic, so a handle can be moved into a `spawn`ed task or used in `par for`; the value is dropped with its last handle. `s.clone()` makes a handle, `let t = s` moves it, so a handle that was moved cannot be used. A `Shared` is read like a `Box` (fields, methods, `match`, indexing, passing it where the held type is borrowed, comparing and printing) but **its contents can never be changed**: `s.push(1)`, `s.x = 5`, `s[0] = 1` and passing `mut s` are errors ("cannot change what is inside a `Shared`"). Data that must change goes in a `Mutex`. A `Shared` is a way to give several tasks, or several versions of a data structure, the same data without copying it (see the persistent tree in `examples/checked/shared.tau`).
- **`Mutex<T>`** holds one value that is changed under a lock. `m.get()` copies the value out (`T` must be cloneable), `m.set(x)` replaces it, `m.update(|v| new)` replaces it with the closure's result, `m.with(|v| result)` reads it and returns something, and `m.into_inner()` takes the value out (the mutex is gone). Its methods take `self` by borrow, so a `Shared<Mutex<T>>` can be used from every task at once. A closure gets the value **by borrow** (like every closure): to change a vector, build a new one (`let mut w = v.clone() ... w`); changing it in place under the lock is not possible yet. A `Mutex` has one owner: it cannot be cloned or printed.
- **Using a mutex inside its own `update`/`with` is a deadlock**, reported as the panic `deadlock: the mutex is already locked by this thread` (on both backends). Locks of different mutexes taken in different orders by different tasks can still deadlock; this is not checked. A task that panics lets go of the locks it holds.
- **Determinism.** `Shared<Mutex<T>>` is an *escape hatch*: tasks lock in whatever order they happen to run, so the result is the same whatever the thread count **only when the updates do not depend on the order** (adding to an integer counter, taking a maximum, inserting into a set), not for a floating-point sum or a log that keeps arrival order. For those, return a value from each task and combine them after `join()`, or use `par sum`, whose order is fixed. The interpreter runs tasks one after another, so it always shows one valid order.
- `x.clone()` now works on every cloneable type (vectors, strings, boxes, tuples, structs, `Shared` handles). `match` and indexing look through a `Box` or a `Shared`.

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

#[test]
fn parses() -> Result<(), str> {
    let n = parse_digit('1')?
    assert_eq(n, 1)
    Ok(())
}

#[test]
#[should_panic("overflow")]
fn too_big() { ... }
```
`tau test file.tau [filter]` checks the program (type and borrow errors stop it before any test runs)
and runs every top-level `#[test]` function whose name contains `filter`, one after another, each in a
fresh scope. A test passes if it finishes; it fails if it panics (a failed `assert`/`assert_eq`, an
integer overflow, an out-of-range index, ...) or returns `Err`. `#[should_panic]` inverts that, and
`#[should_panic("text")]` also requires the panic message to contain `text`. A failing test shows what
it printed and why it failed; output from passing tests is kept quiet. A failing test never stops the
ones after it. The exit code is `0` if all passed, `1` otherwise. `main` is not run. A test takes no
parameters, is not generic, and returns nothing or a `Result`.

---

## 8c. Time (`std.time`)

```
use std.time.*

let start = monotonic_ns()
work()
print("took {format_duration(monotonic_ns() - start)}")     // took 3.266 ms
print(format_iso(unix_ns() / 1_000_000_000))                // 2026-11-19T08:07:53Z
```

Times are plain integers, so they need no new types and behave the same on both backends.

| Function | |
|---|---|
| `monotonic_ns() -> i64` | A clock for timing: nanoseconds since an arbitrary moment. It never goes backwards; only differences mean anything. |
| `unix_ns() -> i64` | The wall clock: nanoseconds since 1970-01-01 00:00:00 UTC. It can jump when the system clock is set. |
| `sleep(secs: f64)` | Wait. Panics on a negative or NaN number, or more than 1e9 seconds. |
| `civil(unix_secs) -> (year, month, day, hour, minute, second)` | The UTC date and time of a Unix time (`i64` values; month 1-12). |
| `from_civil(year, month, day, hour, minute, second) -> i64` | The Unix time of a UTC date. Panics with `invalid date or time` for one that does not exist (month 13, 29 February in a common year, hour 24). Leap seconds are not represented. |
| `weekday(unix_secs) -> i64` | 0 is Monday, 6 is Sunday. |
| `format_iso(unix_secs) -> String` | `2026-11-19T07:13:02Z` (a year before 0 gets a minus sign). |
| `format_duration(ns) -> String` | `950 ns`, `1.500 µs`, `12.345 ms`, `1.250 s`, `2m 05s`, `1h 02m 05s`; truncated, not rounded. |

The calendar is the proleptic Gregorian calendar in UTC, with whole seconds (`civil` and the others take seconds, so
divide `unix_ns()` by `1_000_000_000`). Local time zones are not supported yet. The clocks have the resolution of
the operating system (nanoseconds on Linux, about 100 ns on Windows). They can be used in `par` regions and tasks;
what they return is of course different on every run. A nicer `Instant`/`Duration`/`DateTime` layer is planned for
when the standard library can be written in Tau itself.

---

## 8d. Formatting values

`print("...")` and string literals interpolate expressions with `{expr}`. After a `:` comes a format that says how to write the value:

```
print("pi is about {pi:.3}")              // pi is about 3.142
print("[{n:>6}] [{n:<6}] [{n:^6}]")       // [    42] [42    ] [  42  ]
print("{n:06} {n:+} {n:#x} {n:08b}")      // 000042 +42 0x2a 00101010
print("{x:12.4e} {x:E}")                  // "      3.1416e0 3.14159265E0"
print("{name:*^11}")                      // ****Tau*****
```

```
format = [[fill]align] [+] [#] [0] [width] [.precision] [type]
fill   = any character      align = < (left)  ^ (centre)  > (right)
type   = x X b o            hexadecimal, binary, octal (integers)
       | e E                scientific notation (floats): 1.5e3, 1.50E3
```

- **width** is the least number of characters (not bytes); wider values are never cut. Numbers align right by default, text and everything else left.
- **`.N` on a float** writes exactly N digits after the point, rounding the exact value (so `{2.5:.0}` is `2` and `{0.125:.2}` is `0.12`: ties go to the even digit). **On text** it keeps the first N characters. It is an error on integers.
- **`+`** shows the sign of positive numbers. **`0`** fills a number with zeros after its sign and prefix, up to the width (it replaces fill and alignment). **`#`** adds `0x`, `0X`, `0b` or `0o` to `x`, `X`, `b`, `o`.
- **`x X b o`** show the bits of the integer in its own width: `{-1i8:x}` is `ff`.
- **`e`/`E`** without a precision uses the shortest digits that read back exactly: `1500.0` is `1.5e3`. NaN and infinity are written `NaN` and `inf`, never padded with zeros.
- A complex number takes a precision and `e`, which apply to both parts: `{z:.2}` is `1.50-2.00i`. Anything else that can be printed (vectors, tuples, your own types) takes only a width, fill and alignment.
- A format that does not fit the value (a precision on an integer, `x` on a float, `0` on text) is a compile error. The colon is taken as a format only if what follows it is a valid one, so `{f(|x: i64| x)}` and `{m["a:b"]}` still work.
- A format has no way to take its width or precision from a variable yet.

---

## 9. Modules and packages

### Modules

One file is one module; a folder with `mod.tau` is a module whose submodules are the files in it.

```
orbits/
  tau.toml
  src/
    main.tau          the entry module of a program
    bodies.tau        use bodies
    geo/mod.tau       use geo
    geo/shapes.tau    use geo.shapes
```

```
use bodies                          // the module: bodies.EARTH, bodies.Body(...)
use bodies.{Body, EARTH}            // names from it
use bodies.Body as Planet           // renamed
use geo.shapes.*                    // every public name
use math.fft.{fft, ifft}            // the library: std, math

pub fn visible() { ... }            // usable from other modules
fn hidden() { ... }                 // private to this module
```

- **`use` paths start at the root of the package** (`src/`), never relative to the file they are in. The first name decides where to look: a module of this package, then a dependency from `tau.toml`, then the compiler's library (`std`, `math`). A name that is both a local module and a dependency is an error; names are case-sensitive on every system.
- **Privacy.** Everything is private to its module unless it is `pub`: functions, structs, enums, traits, constants, type aliases, methods of an `impl` block (methods of a trait are as visible as the trait), and the fields of a struct. A struct whose fields are not all `pub` cannot be built or taken apart by `Name(...)` outside its module; the module can offer a `pub fn` that builds it. Enum variants are as visible as the enum.
- **No re-exports yet.** `use` brings a name into the module that wrote it, and nothing else can import it from there; import it from where it is defined. (`pub use` is planned.)
- A module may use any other module, including in a circle. A program is checked as a whole: all its modules are type-checked and borrow-checked together and run by both the interpreter and the native backend.
- Errors name the file (`src/util.tau:12:5`). `#[test]` functions in any module of a package run with `tau test`; a test in a module other than the entry is named `module::test`.
- A single `.tau` file with no `tau.toml` can still use the files next to it (`tau run main.tau` finds `util.tau` for `use util`).

### Packages: `tau.toml`

```
[package]
name = "orbits"
version = "0.1.0"

[dependencies]
kepler = { path = "../kepler" }
fastmath = { git = "https://example.com/fastmath", tag = "v1.2" }

[native]
link = ["openblas"]        # recorded; C interop is not available yet
```

- A package is a folder with a `tau.toml` and a `src/` folder. A **program** has `src/main.tau` (with `fn main()`); a **library** has `src/lib.tau`, which is what `use name` gives to a program that depends on it (`use name.sub` is `src/sub.tau`). Inside a package its own `use` paths are looked up in that package first, so a library's modules do not clash with a program's.
- Dependencies are `{ path = "..." }` (a folder, relative to the `tau.toml`) or `{ git = "URL" }` with at most one of `tag`, `branch`, `rev`. The name on the left is the name used in `use`; it must be an identifier. One name stands for one package in a whole program (two different packages with the same name are an error). A registry (`kepler = "1.2"`) comes later.
- **Git dependencies** are fetched with the `git` command into `$TAU_HOME` (default `~/.tau`) and pinned in **`tau.lock`**, next to `tau.toml`: the exact commit, so a moved tag or branch does not change a build. Commit `tau.lock`. A branch (or a dependency without a reference) stays at its pinned commit until `tau fetch --update`.
- Commands: `tau new name [--lib]`, `tau add name --path DIR | --git URL [--tag T | --branch B | --rev R]`, `tau fetch [--update]`, and `tau check | run | build | test [path]`, where `path` is a `.tau` file, a package folder, or nothing (the package in the current folder or above it). Editors use `tau check - --json --file PATH`.

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

**Types may not contain themselves.** `enum List { Nil, Cons(i64, List) }` has no finite size and is an
error, whether the recursion is direct, through `Option`, a tuple, an array, a generic wrapper, or between
two types. Recursion through a `Vec` is fine (`struct Tree { children: Vec<Tree> }`). A `Box` type is
not available yet.

**Not checked yet** (deliberately deferred): `pub`/privacy across modules (there is one file today);
duplicate or overlapping impls (the first match wins); associated types; closure capture analysis;
`struct`/`enum`/`impl` declared inside a function; compile-time checking of symbolic tensor
dimensions; ownership and borrowing (stage 3).

---

## 14c. Ownership and borrowing (settled while writing the stage-3 checker)

Tau has no reference *types*; borrows are parameter modes. So a borrow only exists (a) for the
length of a call, (b) inside a **view** value, and (c) for the length of a loop or a task.

**Moves.** A value of a type that is not `Copy` is *moved* when it is assigned (`let b = a`), returned,
stored in a struct/array/vector literal, passed to an `own` parameter, or moved into a task or
closure. Using it afterwards is an error ("use of moved value"), reported once per move with a note
at the move. This is tracked per path: after `let n = p.name`, `p.age` is still usable but `p` and
`p.name` are not. Moving on one branch of an `if`/`match`, or inside a loop, counts as moved
afterwards. Assigning to a variable gives it a value again.

**What is `Copy`.** Numbers, `bool`, `char`, `str`, slices, tuples and arrays of `Copy`, `Option`,
`Result` and `Range` of `Copy`, and any type with an explicit `impl Copy for T {}`. Everything else
(`Vec`, `String`, tensors, closures, your structs by default) is moved.

**Borrowed parameters cannot be given away.** Plain and `mut` parameters are borrowed from the
caller, so you cannot move out of them or out of their fields: `fn f(p: Person) -> String { p.name }`
is an error. Write `own p: Person` to take ownership. The same goes for `for`/`match` bindings over
a place (`for s in names`, `Some(s)` on a variable): they are views of the original and cannot be
moved out of. Moving a value out of a collection (`let s = v[0]`) is an error unless the element is
`Copy`.

**Loans.** Calls borrow their arguments: a plain parameter takes a shared loan, `mut` a mutable one.
Loans for one call are created just before the call, after the other arguments are evaluated, so
`v.push(v[0])` is fine but `f(mut v, v)` is an error. Two loans conflict if their places overlap
(`p` and `p.x` overlap; `p.x` and `p.y` do not) and one is mutable. Array indices all count as one
place.

**Views.** A value that can refer to other data (a slice, `str`, a closure, a task, or anything that
contains one) *holds the loans* it was made from. `let s = v[0..2]` makes `s` hold a loan on `v`,
and `s` keeps it for as long as `s` is used later (borrows end at their last use, not at the end of the
block). While the loan is alive you cannot modify, move or mutably borrow `v`. A function that
returns a view is assumed to borrow from all its arguments. A mutating call can store the views
its other arguments carry into the thing it mutates (`views.push(v[0..2])`), but only if that thing
can hold views (a `Vec<[T]>`, not a `[f64]`).

**Loops.** `for x in v` borrows `v` for the whole loop and `for x in mut v` borrows it mutably, so the
body cannot modify (resp. touch) `v`. `for i in 0..v.len` borrows nothing.

**Closures** capture by the least they need: by move if the body moves the variable, by mutable
borrow if it changes it, otherwise by shared borrow. The closure holds those loans while it is used.

**Views and scope.** A view cannot outlive the value it views. A view of a temporary (`let s = first(vec[1, 2, 3])`, where `first` returns a view) is an error, because the temporary is dropped at the end of the statement; so is a view of a variable declared in an inner block that is still used after the block. Use it within the statement, or copy the data (`to_vec`, `String.from`).

**Returning borrows.** A function may only return a view of its own borrowed parameters, never of a
local variable or an `own` parameter.

**Tasks.**
- `spawn` captures like a closure, but may not modify anything it captures (it should return a
  result), and a captured value it consumes is moved in. The task handle holds its captures' loans
  until it is joined, or until it goes out of scope (an unjoined handle is joined there).
  `join` takes the handle by value, so joining twice is a use of a moved value.
- `par for` iterations may write the outer collection only as `out[i]` with the loop's own index.
  Any other write to something declared outside the loop (including `total += ..` and
  `out.push(..)`) is an error, and so is *reading* `out[j]` for any other index when the loop also
  writes `out`. Variables declared inside the body are private to the iteration. Use
  `par for c in par_chunks(mut out, n)` to give each iteration its own mutable slice, and
  `par sum(..)` to accumulate. A parallel reduction body, and an `on` block, may only read.

**Known limits of the v1 checker** (so you know what it does *not* promise):
- A function returning a view is treated as borrowing from *all* its arguments, which can reject a
  function that returns a view of only one of them.
- Slices are copyable views, so a `mut [T]` can be copied; aliasing of mutable views through such
  copies is not tracked.
- All array indices are one place, so `f(mut v[0], mut v[1])` is rejected even though it is safe.
- Closures called through a function value take their arguments by borrow only.
- A returned view derived from a `for`/`match` binding is assumed to come from the caller.
- `par for` cannot see races through function calls (`helper(mut out)` is rejected as a mutable
  borrow, but a helper that reads `out` while another iteration writes it is not detected).
- `on` serialisation rules and cluster semantics are not part of v1 and are not checked. A `Shared<Mutex<T>>` is not checked for lock order or for updates that depend on it.

---

## 14d. Running programs (the interpreter and the native backend)

`tau run file.tau` checks a program (types, then borrowing) and, only if it is accepted, runs it: natively
(translated to C and built with the system `clang` or `gcc`, cached by content) when it can, otherwise
on the interpreter, which is the reference implementation. `--native` and `--interp` force one; both
must print exactly the same thing, and the test suite checks that. Errors stop it before anything executes. Exit codes: `0` normal, `1` rejected or `main` returned an
`Err`, `101` panic.

**Numbers.** Each integer type has its own range. Arithmetic that leaves the range **panics**
("integer overflow") in every build; `as` between integer types wraps, from float to integer
saturates (NaN gives 0), and division by zero panics. `f32` results are rounded to single precision.
Integer `^` panics on a negative exponent. A float prints as the shortest digits that read back
exactly (for an `f32`, exactly *as an `f32`*: `0.1f32` prints `0.1`), never in scientific notation, and
with `.0` added when it is a whole number, whatever its size (`1.0e16` prints `10000000000000000.0`).

**Values.** `Copy` aggregates (tuples, arrays, `impl Copy` structs) are copied when read from a
variable; `Vec`, `String`, maps and tensors are moved, never copied. Arguments for `mut` parameters
are references to the caller's variable, field or element, so assigning to a `mut` parameter
replaces the caller's value. `for x in mut v` binds `x` to a reference to each element.

**Maps.** Reading `m[k]` on a missing key panics. `m[k] = v` adds the entry or overwrites it, and `m[k] += x` (or any other compound assignment) starts a missing entry at zero. Entries keep their insertion order.
`top(n)` returns the `n` entries with the largest values, ties ordered by key.

**Tensors** are stored column-major. `random` uses a fixed seed, so runs are reproducible.
`math.linalg.eig` handles symmetric matrices only (Jacobi rotations), and `math.fft` uses a radix-2
transform when the length is a power of two, otherwise the direct O(n^2) sum.

**Parallel reductions have a fixed evaluation order.** `par sum`, `prod`, `min`, `max`, `any` and `all`
cut the iteration into blocks of 4096 items, reduce each block in order starting from its first item,
and combine the block results in block order. This is the definition of the result, so it never depends
on how many threads run it: a float `par sum` prints the same digits on any machine, and differs from a
plain left-to-right loop in the last digits for long sums.

**Concurrency.** `par for`, `par sum`... and `spawn` run on a pool of threads in a native build (`TAU_THREADS=n`
sets how many; the default is every core) and **one after another on a single thread** in the interpreter.
A program means the same thing either way, whatever the thread count, because:
- reductions have the fixed order above;
- what a parallel iteration or a task prints is kept apart and shown in iteration order (a `spawn`ed task's
  output appears when the task is **joined**, or when its handle is dropped);
- if several iterations panic, the one a sequential run would reach first is reported, after the output
  that precedes it.

A panic inside a task is reported by `join()` (as `Err(TaskPanicked)`) instead of ending the program. A
parallel body cannot `return`, use `?` or `break` out of its loop. There is no cluster yet, so `on` runs
locally (on the interpreter).

**Limits.** Deep recursion ends in a "stack overflow" panic rather than a crash. The interpreter is a
tree walker: roughly 0.5 microseconds per simple expression, so about two million loop iterations per
second. It exists as the reference and for programs the native backend cannot build; the native backend runs
typically 10 to 200 times faster (see `examples/bench/`).

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
