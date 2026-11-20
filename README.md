# Tau

A low-level, math/science-focused programming language with simple syntax (braces, no
semicolons), built-in tensors, local parallelism, and an optional shared-compute layer for
running on named nodes. "Tau" is a working name.

The design lives in [`SPEC.md`](SPEC.md).

## Install

Download the program for your system from the [latest release](https://github.com/pigeon318/Tau/releases/latest)
(`tau-windows-x86_64.exe` or `tau-linux-x86_64`), name it `tau` (`tau.exe`), put it on your `PATH`, and run `tau --version`.
After that `tau update` installs newer releases. A C compiler (MinGW-w64 `gcc` on Windows, `gcc` or `clang` on Linux)
makes programs run natively, 10 to 200 times faster; without one Tau uses its interpreter. [`docs/GUIDE.md`](docs/GUIDE.md)
shows how to use it and lists a tour of programs to run (`examples/tour/`).

## Status

**Version 0.1.0 is the first release.** The compiler reads Tau source, resolves names, infers types, checks traits, generics,
operators and mutability, then checks ownership (use after move, conflicting borrows, views that outlive their data,
race-free `spawn` and `par for`). A **native backend** (Tau to C, compiled by the system `clang` or `gcc`) and an
**interpreter** run the result and agree on every test; the native one runs everything except `on` blocks and a few corner
cases, typically 10 to 200 times faster, and the interpreter is the reference and the fallback (used automatically when a
program needs something native cannot do, or no C compiler is installed). The language has structs, enums, traits,
generics, closures, iterators, modules and packages, `Box`/`Shared`/`Mutex`, checked integers, complex numbers, tensors of
floats, complex numbers, integers and `bool`, linear algebra, the FFT, and thread-pool parallelism whose results never depend
on the number of cores. The standard library covers `Vec`, strings, `Map`, `Set`, `Deque`, `Heap`, random numbers, dates and
clocks, and files. What 0.1.0 contains and what it does not is in
[`docs/release-notes/v0.1.0.md`](docs/release-notes/v0.1.0.md); what comes next is in [`docs/ROADMAP.md`](docs/ROADMAP.md).
See [`docs/native/DESIGN.md`](docs/native/DESIGN.md) for how the native backend works.

| Stage | Deliverable | State |
|---|---|---|
| 1 | Lexer, parser, AST | done |
| 2 | Name resolution, type checker | done |
| 3 | Borrow checker | done |
| 3.5 | Interpreter (`tau run`) | done |
| 4 | Native code generation via C | done |
| 5 | Standard library, tensors | done for 0.1.0 (`path`/`env`/`process` and JSON next) |
| 6 | Thread pool, `par` | done |
| 7 | Modules and packages (`tau.toml`, `tau.lock`), `tau test` | done (`tau fmt` next) |

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

1. **A prebuilt `tau.exe`**: download `tau-windows-x86_64.exe` from the latest
   [release](https://github.com/pigeon318/Tau/releases) (rename it `tau.exe` and put its folder on your `PATH`).
   Every push also builds one in GitHub Actions (the `ci` workflow, artifact `tau-windows-x86_64`).
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

## Modules and packages

A program can be many files and can use other packages (SPEC.md, section 9):

```bash
tau new orbits              # a package: tau.toml and src/main.tau
cd orbits
tau add kepler --path ../kepler
tau run                     # use kepler  /  use bodies  /  use geo.shapes.{Circle, area}
tau test                    # the #[test] functions of every module
```

Names are private to their module unless `pub`; `use` paths start at the package's `src/` folder. Git
dependencies (`tau add x --git URL --tag v1`) are pinned in `tau.lock`. See `examples/projects/` for a program
with a dependency.

## Updating

`tau update` replaces the program with the newest release: it downloads the file for your platform, checks it against
the release's `SHA256SUMS`, runs it once, and only then swaps it in (a damaged or broken download changes nothing).
`tau update --check` only says whether a newer version exists, and `tau update --force` installs the latest release
even if you already have it. It needs `curl` (Windows 10 and later include it). If the repository is private it also needs a
GitHub token that can read it: set `GITHUB_TOKEN`, or install the GitHub CLI and run `gh auth login`. A `tau` built from
source (in a `target` folder) is updated with `git pull` and `cargo build --release` instead.

Releases are made by the `release` workflow: run it from the Actions tab (it uses the version in `Cargo.toml` and the notes in `docs/release-notes/`), or push a tag that matches that version (`git tag v0.1.0 && git push origin v0.1.0`);
the `release` workflow builds Windows and Linux programs and attaches them.

## Editor support

`editors/vscode/` is a VS Code extension: highlighting, snippets, live errors from the compiler, types on hover, and Run / Run test links.
Build it with `npm install && npm run package` in that folder and install the `.vsix` (`code --install-extension`), or download `tau-vscode-extension` from the CI run.
It needs `tau` on the PATH (or the `tau.path` setting).

## Checking a change without CI

GitHub Actions minutes are limited, so everything the workflow checks can also be run on your own machine:

```bash
scripts/check.sh           # the full test suite, the VS Code tests, and a cross-build of tau.exe (Linux)
scripts/check.sh quick     # the fast test binaries only (about two minutes)
scripts/check.sh memory    # also the native tests under valgrind, UBSan and ThreadSanitizer
```

```powershell
powershell -ExecutionPolicy Bypass -File scripts\windows-check.ps1    # the Windows tests, on a Windows PC
```

Releases are built by the `release` workflow (one run per release, started by pushing a `v*` tag or by hand).

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
| `src/project.rs`, `src/toml.rs`, `src/gitdeps.rs` | Packages: finding and loading the files of a program, `tau.toml`, dependencies and `tau.lock` |
| `src/workspace.rs`, `src/source.rs`, `src/unit.rs` | A program as a list of modules, and the map from error positions to files |
| `src/editor.rs` | What editors need: `tau check --json` (diagnostics and hover types, LSP-style positions) |
| `editors/vscode/` | The VS Code extension (grammar, snippets, live diagnostics, hovers, run/test links); see its README |
| `src/prelude/*.tau` | The built-in library's signatures, written in Tau |
| `examples/` | Programs from the spec; every one must parse |
| `examples/checked/` | Realistic programs that must type-check and borrow-check with no errors |
| `examples/tests/` | A program with `#[test]` functions, for `tau test` |
| `examples/run/` | Programs that run, each with a `.expected` output from an independent Python port (`reference.py`) |
| `examples/borrow_errors/` | One program per kind of borrow mistake; each must be rejected with the stated error |
| `tests/` | Parser, type-checker and borrow-checker tests, error-message tests, and no-panic fuzzing |

## Licence

Tau is licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you say otherwise, any contribution you submit for inclusion in Tau is licensed as above, without any additional terms or conditions.
