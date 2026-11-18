# Tau

A low-level, math/science-focused programming language with simple syntax (braces, no
semicolons), built-in tensors, local parallelism, and an optional shared-compute layer for
running on named nodes. "Tau" is a working name.

The design lives in [`SPEC.md`](SPEC.md).

## Status

**Stage 3 of 7: borrow checker.** The compiler reads Tau source, resolves names, infers types,
checks traits, generics, operators and mutability, and then checks ownership: use after move,
conflicting borrows, views that outlive their data, and the rules that make `spawn` and `par for`
race-free. It cannot run anything yet.

| Stage | Deliverable | State |
|---|---|---|
| 1 | Lexer, parser, AST | done |
| 2 | Name resolution, type checker | done |
| 3 | Borrow checker | done |
| 3.5 | Interpreter (run programs) | next (suggested) |
| 4 | LLVM code generation, runtime | |
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
| `src/borrowck/` | The borrow checker: `lower` (AST to an event CFG), `analysis` (liveness and dataflow, the checks), `mir` |
| `src/prelude/*.tau` | The built-in library's signatures, written in Tau |
| `examples/` | Programs from the spec; every one must parse |
| `examples/checked/` | Realistic programs that must type-check and borrow-check with no errors |
| `examples/borrow_errors/` | One program per kind of borrow mistake; each must be rejected with the stated error |
| `tests/` | Parser, type-checker and borrow-checker tests, error-message tests, and no-panic fuzzing |
