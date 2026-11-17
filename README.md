# Tau

A low-level, math/science-focused programming language with simple syntax (braces, no
semicolons), built-in tensors, local parallelism, and an optional shared-compute layer for
running on named nodes. "Tau" is a working name.

The design lives in [`SPEC.md`](SPEC.md).

## Status

**Stage 1 of 7: lexer, parser and AST.** The compiler can read Tau source and produce a syntax
tree with precise error messages. It cannot yet type-check or run anything.

| Stage | Deliverable | State |
|---|---|---|
| 1 | Lexer, parser, AST | done |
| 2 | Name resolution, type checker | next |
| 3 | Borrow checker | |
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
```

## Layout

| Path | Contents |
|---|---|
| `src/lexer.rs` | Tokens, including the newline-as-terminator rules and string interpolation |
| `src/parser.rs` | Recursive-descent parser with precedence climbing |
| `src/ast.rs` | Syntax tree |
| `src/diag.rs` | Error type and rendering with source snippets |
| `src/sexp.rs` | S-expression printer used by tests and `--sexp` |
| `examples/` | Programs from the spec; every one must parse |
| `tests/` | Structure tests, error-message tests, and no-panic robustness tests |
