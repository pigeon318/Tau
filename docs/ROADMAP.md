# Tau: where we are, and the road to the finished language

Written on 2026-11-19, at version 0.1.0. Effort sizes are relative: **S** a few days, **M** one to two weeks, **L** a month, **XL** several months.

---

## 1. What we have

### The language and compiler (done and tested)

| Area | State |
|---|---|
| Front end | Lexer, parser, type checker with inference, generics, traits, operator overloading, exhaustive `match` checking. |
| Ownership | Borrow checker (moves, conflicting borrows, views that outlive data, race-free `spawn` and `par for`). |
| Execution | An interpreter (the reference) and a native backend (Tau to C to clang/gcc, cached). Native is 10 to 200 times faster and falls back to the interpreter when it cannot compile something. |
| Parallelism | `par for`, `par sum`, `spawn`/`join` on a thread pool; results never depend on the number of cores. |
| Maths | `Tensor` (column-major, `@`, elementwise ops), complex numbers, `det`/`inv`/`eig`/`trace`/`transpose`, FFT. Native results match the interpreter bit for bit. |
| Data | tuples, arrays, slices, `Vec`, `String`/`str`, `Map`, `Option`/`Result` with `?`, closures. |
| Std | `std.fs`, `std.io`, `std.map`, `std.time` (clocks, `sleep`, UTC calendar, formatting). |
| Platforms | Linux and Windows 10/11 (x86-64), both tested in CI. |

About 20,000 lines of Rust plus a 1,500-line C runtime; about 420 tests, including a differential fuzzer (interpreter against native), UBSan, TSan and valgrind runs.

### Tooling (done)

`tau run | check | test | build | clean | update`, `tau check --json` for editors, a VS Code extension (highlighting, snippets, live errors, hover types, Run/Test links, outline), CI on Linux and Windows, a release workflow (waiting for the first tag), `tau update`.

---

## 2. What is missing (verified, not guessed)

### Language gaps (the spec promises these for v1)

1. **Modules and multi-file programs.** `use util` fails with "unresolved import". Everything is one file plus the built-in library. *This blocks the most.*
2. **Packages**: no `tau.toml`, `tau new`, `tau add`, lockfile, git or path dependencies.
3. **`Box<T>` and recursive types** (trees, linked lists, ASTs): `enum L { Nil, Cons(i32, L) }` is rejected as infinite size.
4. **`Shared<T>`** (shared ownership): the type does not exist.
5. **Formatting specs**: `"{x:.2}"` is a parse error, so no precision, width or padding. A big gap for scientific output.
6. **Tensor slicing and views**: `t[0..2, 0]` is rejected; tensors of integers are limited; no `t.sum()` method style (only free functions).
7. **Library code written in Tau**: library modules cannot have bodies (the checker, interpreter and code generator only handle the user's module), so std and math must be built into the compiler. Fixing this is the same work as modules (item 1).
8. **C interop** (`extern`, `[native] link = [...]`), and BLAS/LAPACK bindings (our tensor kernels are our own loops).
9. Smaller: `Range` fields (`r.start`) pass the checker but panic in the interpreter; `Option`/`Result` cannot be printed directly; `Map` keys must be the exact key type.

### Tooling gaps

`tau fmt`, native `tau test` (today it runs on the interpreter), `tau doc`, a REPL, a language server (`tau lsp`), a debugger, `tau bench`, a package registry, an installer.

### Platform gaps

macOS (the runtime uses a few Linux-only calls), ARM (Apple silicon, Windows on ARM, Raspberry Pi), WebAssembly.

### Deliberate v1.5 / v2 items

Units of measure, compile-time tensor shapes (v1.5); `on` blocks, clusters, networking, sandboxing (v2); GPU, macros (later).

---

## 3. The finished stack

```
                         ┌───────────────────────────────────────────────┐
  YOU                    │  Tau Workbench (own IDE)  ·  VS Code  ·  web  │
                         └───────────────▲───────────────────────────────┘
                                         │ Language Server Protocol (tau lsp)
  TOOLS     tau  new · add · build · run · test · bench · fmt · doc · lint · repl · update · publish
                                         │
  PACKAGES  registry  ·  tau.toml / tau.lock  ·  git and path dependencies
                                         │
  LIBRARIES (written in Tau, compiled with your program)
            core ─ std ─ math ─ science ─ data ─ viz        (section 4)
                                         │
  COMPILER  lexer → parser → types → borrow check → IR → ┬─ C backend  → clang/gcc  (native)
                                                          ├─ interpreter (reference, REPL, playground)
                                                          └─ later: LLVM or Cranelift, WebAssembly
                                         │
  RUNTIME   thread pool · tensors · allocator · panics · (later: cluster, GPU)
```

Design rules that stay fixed: one fast native path and one reference interpreter that must agree; results of parallel code never depend on the core count; nothing hidden (no GC, no implicit numeric conversions); every feature arrives with differential tests.

---

## 4. The libraries we will make

All are packages in Tau once modules exist, grouped by layer. `std` ships with the compiler; the rest ship as packages from the registry (some bundled).

**core** (no allocation): integers and floats with `wrapping_*`/`checked_*`/`saturating_*`, `Option`, `Result`, `Range`, traits (`Clone`, `Eq`, `Ord`, `Hash`, `Display`, `Default`, operator traits), `Iterator` and lazy adapters (`map`, `filter`, `zip`, `enumerate`, `fold`, `windows`, `chunks`).

**std**: `Vec`, `String`, `Map` (hashed and ordered), `Set`, `Deque`, `Heap`, strings and Unicode, formatting specs, `fs`, `path`, `env`, `process`, `io` (buffered), `time` (adds `Instant`, `Duration`, `DateTime`, time zones), `random` (PRNGs and distributions, seedable), threads, `sync` (`Shared<T>`, channels, atomics), `net` and `http` (later), `regex`, serialization (`json`, `csv`, `toml`, a binary format).

**math**: dense `Tensor` with slicing, views, broadcasting and integer/complex/bool element types; linear algebra (LU, QR, Cholesky, SVD, symmetric and general eigenproblems, solves, least squares, norms) on BLAS/LAPACK when available and our own kernels otherwise; FFT (1D, 2D, real); sparse matrices and iterative solvers; special functions (gamma, erf, Bessel); polynomials; interpolation; numerical integration; root finding; optimisation; ODE solvers (RK4, adaptive Runge-Kutta, stiff); automatic differentiation; statistics and distributions; units of measure (v1.5).

**science**: physical constants (CODATA) and units, geometry (vectors, quaternions, transforms), signal processing (filters, windows, spectra), graphs and networks, image I/O, Monte Carlo and sampling helpers, finite differences and meshes.

**data**: a table/dataframe type, CSV and Parquet readers, grouping and joins, plain statistics over columns.

**viz**: plots to SVG/PNG (line, scatter, heatmap, histogram), shown inline in the IDE and notebooks.

---

## 5. The IDE

**Decision: do not build a text editor from scratch.** The expensive, valuable part is language intelligence, and it is shared by every editor. So:

1. **`tau lsp`** (language server): diagnostics, hover, go to definition, references, rename, completion, signature help, formatting, code actions (add `use`, fix borrow errors), inlay hints (inferred types). It reuses the data `tau check --json` already produces. VS Code, Neovim, Helix, Zed and others get Tau support for free.
2. **VS Code extension** (done in basic form): add the language client, debugging (a DAP adapter over gdb/lldb using `#line` mapping), a test explorer, a tensor viewer.
3. **Tau Workbench** (own IDE), built on the same server, as a Tauri app (Rust core, web UI) with the Monaco editor. It is not a copy of VS Code; it exists for what scientists need and generic editors lack:
   - run cells and notebooks (`.taunb`) with inline plots and tensor viewers,
   - a live units and shapes panel (once v1.5 lands),
   - a timing and profiling view fed by `std.time`/`tau bench`,
   - one-click package install and the built-in docs browser,
   - a bundled compiler and C toolchain, so Windows users install one thing.
4. **Web playground**: the interpreter compiled to WebAssembly (cheap, since it is Rust), for the docs site and sharing snippets.

---

## 6. The road, in order

Each milestone ends with tests, examples and a release.

| # | Milestone | What it contains | Size | Why now |
|---|---|---|---|---|
| **M7** ✅ | **Modules and packages** | multi-file modules (`mod.tau`), `use` of user modules and packages, visibility, `tau.toml`/`tau.lock`, `tau new/add`, path and git dependencies; library modules with bodies in checker, interpreter, codegen and borrowck | L | Everything else is blocked on it. |
| **M8** ✅ | **Core language completion** | `Box<T>`, recursive types, `Shared<T>` and sync primitives, formatting specs (`{x:.3}`, `{n:>8}`), iterator protocol and lazy adapters, tensor slicing and views, integer/complex/bool tensors | L | The language stops surprising people. |
| **M9** | **Std in Tau** | move `Vec`/`Map`/`String` helpers, `Instant`/`Duration`/`DateTime`, `random`, `path`/`env`/`process`, `Set`/`Deque`/`Heap`, serialization | L | Most of it is plain Tau code. |
| **M10** | **Daily tooling** | `tau fmt`, native `tau test`, `tau doc`, `tau bench`, REPL, `tau lsp` v1, VS Code client, macOS port | L | The language becomes pleasant. |
| **M11** | **Maths library** | BLAS/LAPACK-class linear algebra, sparse, special functions, ODEs, optimisation, statistics, autodiff, FFT variants | XL | The reason Tau exists. |
| **M12** | **Units and compile-time shapes** (v1.5) | `Quantity<T, unit>`, `Tensor<f64, [N, M]>` checked at compile time, dimension errors in the editor | XL | The feature that sets Tau apart. |
| **M13** | **IDE** | Workbench, notebooks, inline viz, debugger, web playground | XL | Built on the LSP. |
| **M14** | **Ecosystem** | registry, `tau publish`, installers (winget, scoop, brew), docs site, science and data packages | L | Needs M7 and users. |
| **M15** | **Beyond one machine** (v2) | `on` blocks, clusters, serialisation, GPU, LLVM or Cranelift backend, ARM and WebAssembly targets | XL | Only when the rest is solid. |

M7 and M8 are done (modules and packages; `Box`, `Shared`/`Mutex`, formatting, iterators, tensor slicing, integer and bool tensors). The release `v0.1.0` waits for M9 (the basic language complete). M10's language server and M11's maths can be started in parallel with M9 by separate sessions.

---

## 7. Decisions we need to make

1. **Backend.** The spec says LLVM; we went through C. Recommendation: keep C until M13. It is easy to check and debug, and fast enough. Add LLVM or Cranelift in M15 only if compile time or portability demands it (on Windows the C compiler dependency is the cost; bundling a compiler in the installer removes most of it).
2. **Licence.** ✅ Decided: MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`).
3. **Public or private repository.** Going public (planned) removes the token `tau update` needs, makes Actions free, and enables the playground and docs site.
4. **The name.** "Tau" is a working name. It is short but already used by several projects (including a performance-analysis toolkit and other languages); check before public launch, because the name decides the command, the package registry and the file extension.
5. **Where the IDE lives.** One repository with the compiler (simple, versions together) or a separate one. Recommendation: separate repository from M13 on, depending on a released `tau lsp`.
6. **Platform priority.** Windows and Linux today; macOS in M10; ARM in M15. Say if a Mac or a Raspberry Pi matters sooner.

---

## 8. What I would do next

M7. Modules are the key that opens everything: user programs of more than one file, packages, and writing the standard library in Tau. Concretely: represent a program as several parsed modules, check and borrow-check them all, let the interpreter and code generator register every module's functions, and add `tau.toml`. After that, the library work in M9, M11 and M14 can be done in parallel.
