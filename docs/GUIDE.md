# Using Tau

Tau is a small, fast language for maths and science: braces, no semicolons, checked integers, tensors, complex numbers, and
parallel loops whose results never depend on how many cores you have.

## Install

Download `tau.exe` (Windows) or `tau` (Linux) from the latest release on GitHub, put it in a folder on your `PATH`, and run
`tau --version`. After that, `tau update` keeps it current. Tau compiles to native code through C when a C compiler is
installed (MinGW-w64 `gcc` on Windows, `gcc` or `clang` elsewhere); without one it runs everything in its interpreter,
which gives the same answers, only slower.

## The commands

```
tau run file.tau            check a program and run it (natively when possible)
tau run file.tau --interp   run it with the interpreter (the reference; slower)
tau check file.tau          find type and ownership errors without running
tau build file.tau          compile to an executable and print where it is
tau test file.tau           run the #[test] functions in it
tau new myproject           a new project (tau.toml, src/main.tau)   [--lib for a library]
tau add name --path DIR     depend on another package  (or --git URL)
tau update                  install the newest release       (--check only asks)
tau clean                   empty the build cache
```

`tau run` with no file runs the project in the current folder. Editor support (colours, errors as you type, hover types,
run and test buttons) comes from the VS Code extension in the release (`tau-vscode-extension.vsix`).

## A first program

```tau
fn main() {
    let name = "world"
    print("Hello, {name}!")
    for i in 1..=3 {
        print("{i} squared is {i ^ 2}")
    }
}
```

Save it as `hello.tau` and run `tau run hello.tau`. Things to notice: a new line ends a statement (no semicolons),
`{expr}` inside a string prints a value (`{x:.3}` rounds to 3 decimals, `{n:>8}` right-aligns, `{n:08}` pads with zeros,
`{n:x}` is hex), and `^` is power (`^^` is xor).

## The language in a page

```tau
let x = 5                       // immutable; `let mut x` to change it
let v = vec[3, 1, 2]            // a growable vector;  [1, 2, 3] is a fixed array
struct Point { x: f64, y: f64 } // and enums, traits, generics, impl blocks
fn add(a: i64, b: i64) -> i64 { a + b }
let squares = (1..=10).iter().map(|k| k * k).sum()   // lazy iterator chains
let t = Tensor<f64, [_, _]>.zeros(3, 3)              // tensors; also i64, bool, c128 elements
let big = par sum(i in 0..1000000) i % 7             // parallel sums, same digits on any machine
let h = spawn { heavy_work() }                       // tasks; h.join() gives the result
```

- **Ownership, not garbage collection.** A value has one owner; passing it moves it, `f(x)` borrows it, `f(mut x)` lets the
  function change it. The compiler checks this, so there are no dangling pointers and no data races. `Shared<T>` and
  `Mutex<T>` are there when several owners are really needed. A mistake gives a message that says what and where.
- **Integers are checked.** `255u8 + 1` panics instead of wrapping.
- **Text is UTF-8.** `s.split(",")`, `s.trim()`, `s[1..4]`, `"42".parse_i64()`, `s.chars()`, `parts.join(", ")`.
- **Collections:** `Vec`, `Map` (`use std.map.Map`), and `Set`, `Deque`, `Heap` (`use std.collections.Set`).
- **Maths:** `sqrt`, `sin`, ..., `PI`; `use math.linalg` (`inv`, `det`, `eig`), `use math.fft.{fft}`; complex numbers
  `3.0 + 4.0i` with `.re` and `.im`.

The whole language is described in `SPEC.md`, and `examples/checked/` has a short program for each feature.

## A tour: programs to run

Each one is in `examples/tour/`; run it with `tau run examples/tour/NAME.tau`.

| Program | What it shows |
|---|---|
| `01_hello` | values, functions, structs, formatted tables |
| `02_primes_and_numbers` | a prime sieve with `bool` tensors, twin primes, Collatz, iterator chains, checked integers |
| `03_mandelbrot` | the Mandelbrot set in ASCII, with complex numbers |
| `04_game_of_life` | Conway's Game of Life with whole-grid tensor operations (a glider and a blinker) |
| `05_pi_in_parallel` | pi three ways with parallel sums; same digits for `TAU_THREADS=1` and `=8` |
| `06_linear_algebra` | solving equations, determinants, eigenvalues, and the Fourier transform of a two-tone signal |
| `07_heat` | heat spreading along a rod, with tensor slices |
| `08_words` | text processing: word counts, sorting, parsing numbers, palindromes |
| `09_orbit` | a planet on an elliptical orbit, and the energy a leapfrog integrator conserves |
| `10_tasks` | `spawn`, a shared `Mutex` counter, and a parallel loop |
| `speed` | timings: sorting a million numbers, a matrix product, a 50-million-term parallel sum |

Try `tau run examples/tour/speed.tau`, then the same with `--interp` and a smaller size, to see what native code buys you.
For more, `examples/checked/` has `iterators`, `strings`, `shared`, `slices`, `trees` and others, and `examples/projects/`
has two packages that use each other (`tau run examples/projects/orbits`).

## Learning more

- `SPEC.md`: the language, section by section, with every rule (ownership, formats, modules, the standard library).
- `docs/ROADMAP.md`: where Tau is going.
- `README.md`: building from source, the tests, and how releases are made.
