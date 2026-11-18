# Tau

A low-level, math/science-focused programming language with simple syntax (braces, no
semicolons), built-in tensors, local parallelism, and an optional shared-compute layer for
running on named nodes. "Tau" is a working name.

The design lives in [`SPEC.md`](SPEC.md).

## Status

**Tau programs run.** The compiler reads Tau source, resolves names, infers types, checks traits,
generics, operators and mutability, then checks ownership (use after move, conflicting borrows,
views that outlive their data, race-free `spawn` and `par for`), and an interpreter runs what is
accepted. A **native backend** (Tau to C, compiled by the system `clang`/`gcc`) is under way: it
currently handles everything except closures, tensors, complex numbers and parallelism (so: scalars, structs,
enums, generics, traits, `Vec`/`String`/arrays/slices/`Map`, files, ownership and checked arithmetic),
about 100× faster than the interpreter; the rest still runs on the interpreter, and `par`/`spawn`
run on a single thread. See [`docs/native/DESIGN.md`](docs/native/DESIGN.md).

| Stage | Deliverable | State |
|---|---|---|
| 1 | Lexer, parser, AST | done |
| 2 | Name resolution, type checker | done |
| 3 | Borrow checker | done |
| 3.5 | Interpreter (`tau run`) | done |
| 4 | Native code generation via C (see `docs/native/DESIGN.md`) | milestone 2 of 6 done |
| 5 | Std library, tensors | |
| 6 | Thread pool, `par` | |
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
