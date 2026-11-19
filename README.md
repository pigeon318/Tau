# Tau

A low-level, math/science-focused programming language with simple syntax (braces, no
semicolons), built-in tensors, local parallelism, and an optional shared-compute layer for
running on named nodes. "Tau" is a working name.

The design lives in [`SPEC.md`](SPEC.md).

## Status

**Tau programs run.** The compiler reads Tau source, resolves names, infers types, checks traits,
generics, operators and mutability, then checks ownership (use after move, conflicting borrows,
views that outlive their data, race-free `spawn` and `par for`), and an interpreter runs what is
accepted. A **native backend** (Tau to C, compiled by the system `clang` or `gcc`) runs
everything except `on` blocks and a few corner cases: scalars, structs, enums, generics, traits, closures,
`Vec`/`String`/arrays/slices/`Map`, tensors, complex numbers, linear algebra, FFT, files, clocks and dates (`std.time`), ownership, checked
arithmetic, and `par for`/`par sum`/`spawn` on a thread pool whose results never depend on the number of
cores. It is typically 10 to 200 times faster than the interpreter, which remains the reference and the
fallback (used automatically when a program needs something native cannot do, or no C compiler is installed).
See [`docs/native/DESIGN.md`](docs/native/DESIGN.md).

| Stage | Deliverable | State |
|---|---|---|
| 1 | Lexer, parser, AST | done |
| 2 | Name resolution, type checker | done |
| 3 | Borrow checker | done |
| 3.5 | Interpreter (`tau run`) | done |
| 4 | Native code generation via C (see `docs/native/DESIGN.md`) | done |
| 5 | Std library, tensors | |
| 6 | Thread pool, `par` | done (part of stage 4) |
| 7 | Package manager, `tau test`, `tau fmt` | |

## Try it

Requires Rust.

```bash
cargo build
cargo test

./target/debug/tau parse examples/nbody.tau            # check that a file parses
./target/debug/tau parse examples/nbody.tau --sexp     # show function bodies as S-expressions
./target/debug/tau parse examples/nbody.tau --ast      # full AST
./target/debug/tau tokens examples/nbody.tau           # token stream

./target/debug/tau check examples/checked/nbody.tau            # type-check and borrow-check
./target/debug/tau check examples/checked/nbody.tau --types    # ... and show every inferred type

./target/debug/tau run examples/run/primes.tau                 # check, then run
cargo build --release && ./target/release/tau run examples/run/nbody.tau   # much faster

./target/debug/tau test examples/tests/stack.tau               # run the #[test] functions

# native backend (needs clang or gcc)
./target/release/tau run examples/run/collatz.tau              # native when the program is supported, else interpreted
./target/release/tau run examples/run/collatz.tau --native     # native only: say why if it cannot be compiled
./target/release/tau run examples/run/collatz.tau --interp     # the reference interpreter
./target/release/tau build examples/run/collatz.tau --emit-c   # show the generated C
```

## Windows

Tau runs on Windows 10 and 11 (64-bit). There are two ways to get it:

1. **A prebuilt `tau.exe`**: every push builds one in GitHub Actions (the `ci` workflow, artifact
   `tau-windows-x86_64`).
2. **From source**: install Rust with `rustup` (the default MSVC toolchain, which needs the "Desktop
   development with C++" build tools), then `cargo build --release`; the program is
   `target\release\tau.exe`.

By itself `tau.exe` runs programs with the **interpreter**. For the native (fast) backend it needs a C
compiler on the `PATH`. The easiest is MinGW-w64 gcc from [MSYS2](https://www.msys2.org/): install MSYS2,
run `pacman -S mingw-w64-ucrt-x86_64-gcc` in its UCRT64 shell, then add `C:\msys64\ucrt64\bin` to your
`PATH` (LLVM clang built for MinGW also works). `tau --version` shows which compiler it found, and
`tau run -v file.tau` says why a program ran on the interpreter. If no compiler is found, `tau run` says so
in one line and uses the interpreter.

Differences from Linux: compiled programs are cached in `%LOCALAPPDATA%\tau\cache`; error messages from the
operating system (a missing file, say) are Windows's; a stack overflow inside a `spawn`ed task ends the
program with the usual panic instead of being reported by `join`; and maths functions (`sin`, `exp`, `^` on
floats) come from the system's C library, so their last digits can differ from Linux's. The interpreter and
the native backend still agree with each other on the same machine.

## Layout

| Path | Contents |
|---|---|
| `src/lexer.rs` | Tokens, including the newline-as-terminator rules and string interpolation |
| `src/parser.rs` | Recursive-descent parser with precedence climbing |
| `src/ast.rs` | Syntax tree |
| `src/diag.rs` | Error type and rendering with source snippets |
| `src/sexp.rs` | S-expression printer used by tests and `--sexp` |
| `src/types.rs`, `src/infer.rs` | Semantic types; unification with integer/float literal variables |
| `src/program.rs` | What is known about structs, enums, traits, functions and impls |
| `src/check/` | The checker: `collect` (names, imports, signatures), `lower` (types), `traits` (impl and method lookup), `expr`, `pat` |
| `src/interp/` | The interpreter: `eval` (expressions, patterns), `builtins`, `tensor` (matrices, linear algebra, FFT), `value` |
| `src/native/` | The native backend: `cg` (checked AST to C), `runtime.h` (the C runtime), and the driver that compiles, caches and runs |
| `src/borrowck/` | The borrow checker: `lower` (AST to an event CFG), `analysis` (liveness and dataflow, the checks), `mir` |
| `src/prelude/*.tau` | The built-in library's signatures, written in Tau |
| `examples/` | Programs from the spec; every one must parse |
| `examples/checked/` | Realistic programs that must type-check and borrow-check with no errors |
| `examples/tests/` | A program with `#[test]` functions, for `tau test` |
| `examples/run/` | Programs that run, each with a `.expected` output from an independent Python port (`reference.py`) |
| `examples/borrow_errors/` | One program per kind of borrow mistake; each must be rejected with the stated error |
| `tests/` | Parser, type-checker and borrow-checker tests, error-message tests, and no-panic fuzzing |
