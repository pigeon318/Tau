# Tau native backend: design proposal

**Status:** design approved (decisions in §19). Milestone 0 (prerequisites, §14) and **milestone 1**
(scalars, tuples, strings, functions, control flow, checked arithmetic, panics, `print`) are done; see §17
for what M1 turned out to need. The measurement spikes are in [`spikes/`](spikes/).

**Summary.** Compile Tau to **C**, then let the system C compiler (`clang`, else `gcc`) produce the
executable. Between the checked AST and the C text sits a small monomorphic, explicitly-dropped IR.
A small C runtime supplies strings, vectors, maps, tensors, formatting and a thread pool. Parallel
reductions get a fixed-block definition so their results are identical for any thread count, and the
interpreter adopts the same definition so the two backends can be tested against each other
output-for-output.

---

## 1. Goals and non-goals

**Goals**

1. **Speed.** Orders of magnitude over the interpreter (measured below), on a single thread.
2. **Real parallelism.** `par for`, `par sum`, `par_chunks` and `spawn` run on all cores. The borrow
   checker already proves them race-free; the backend just has to execute them.
3. **Identical behaviour.** The native program prints exactly what the interpreter prints and fails in
   exactly the same way (overflow, bounds, panics, exit code). The interpreter stays as the
   reference implementation.
4. **Debuggable output.** `tau build --emit-c` gives readable C.
5. **Keep the door open** to other backends (LLVM IR directly, WebAssembly) without redoing the
   front end.

**Non-goals (for the first version)**

- Cluster / `on` (stays "run locally", as in the interpreter), units, compile-time shape checking.
- Cross-compilation, incremental compilation, a package manager.
- A garbage collector. Ownership already tells us when to free.
- Beating hand-tuned BLAS on matrix products.

---

## 2. What the spikes measured

[`spikes/`](spikes/) holds hand-written C, shaped like what a compiler would emit for programs we
already have, **including Tau's safety checks** (overflow detection with `__builtin_*_overflow`,
bounds checks on every index). Same machine (4 cores), `clang -O2`.

| Program | Interpreter | Native (spike) | Speed-up |
|---|---|---|---|
| `integrate`, 10M steps, sequential | 4.55 s | 0.054 s | **84×** |
| `integrate`, 10M steps, 4 threads | 4.55 s | 0.018 s | **250×** (3.9× over 1 thread) |
| `collatz`, to 100,000 (checked ints) | 4.12 s | 0.022 s | **187×** |
| `nbody`, 100,000 steps | 1.42 s | 0.007 s | **200×** |

Three things the numbers say beyond "fast":

- **Safety is cheap enough.** Overflow-checked integer code is ~1.7× slower than unchecked in the
  tightest loop (`collatz`: 0.269 s vs 0.161 s) and is free in float-heavy code. We keep Tau's
  "overflow panics" rule.
- **Results can match exactly.** After 100,000 steps the C n-body prints
  `0.14683541914393228`, the same 17 digits as the interpreter, and the sequential integral
  `1.9999999999998086` matches too. Native and interpreted runs can be compared byte for byte, if
  we build with `-ffp-contract=off` (no fused multiply-add) and never `-ffast-math`.
- **A naive parallel `par sum` is not reproducible.** Floating-point addition is not associative, so
  summing in a different order gives a different answer. The spike's parallel version gives
  `1.9999999999999816` (different from sequential `...998086`) but is **identical for 1, 2 and 4
  threads** because it sums fixed-size blocks and combines them in order. That is the semantics we
  adopt (§9).

---

## 3. Why C (and not LLVM or a VM)

| | **C via `cc`** | LLVM IR via bindings | Bytecode VM |
|---|---|---|---|
| Speed | Full `clang -O2` optimiser (it *is* LLVM) | Same | 5-20× slower than native |
| Build dependency for Tau | None at build time; a C compiler at run time | `llvm-sys`/`inkwell` must match the installed LLVM major version, and `llvm-config` + libraries at build time | None |
| Debuggability | Readable output, `-g`, sanitizers, any debugger | Opaque IR | Own tooling |
| Effort to first program | Lowest | Medium | Medium |
| Risk | Must avoid C undefined behaviour | Binding churn | Never reaches native speed |
| Portability | Anywhere a C compiler exists | Where LLVM does | Everywhere |

**Decision: C.** The machine has clang 18, gcc 13 and `llvm-config` 18, so either route would work
here, but C wins on risk and debuggability and costs nothing in speed. Its real cost is that users
need a C compiler installed, which we accept for now (§17, question 1). Because the IR in §6 is
backend-neutral, an LLVM emitter can replace the C printer later without touching anything before it.

**Avoiding C's traps.** Arithmetic never relies on undefined behaviour: integer ops use
`__builtin_*_overflow`, shifts check the amount, array access is bounds-checked, every variable is
initialised, and we compile with `-fno-strict-aliasing`. A CI mode builds the generated C with
`-fsanitize=address,undefined`, so a compiler bug that produces UB is caught immediately.

---

## 4. Pipeline

```
 source ─► parse ─► type check ─► borrow check ─┬─► interpreter      (reference, today)
                    │   │                        │
                    │   └ Results tables         └─► monomorphize ─► lower to TIR ─► drop elaboration
                    └ Program (types, impls)                                              │
                                                                                          ▼
                                       runtime (C) ◄── C emitter ◄──────────────── TIR (monomorphic)
                                            │              │
                                            └──── cc ──────┴─► executable
```

Everything left of the split already exists and is shared with the interpreter. The new work is the
right-hand column.

---

## 5. What the front end already gives us (and what it lacks)

**Already available**, as side tables keyed by AST node (`check::Results`) plus the `Program`:

- the static type of every expression and pattern (`expr_ty`, `pat_ty`);
- what each name, call, method call, operator and constructor refers to (`expr_res`), **with the
  generic arguments of every call** (`Res::Fn(FnId, Vec<(GenericId, Ty)>)`), which is exactly the
  input monomorphization needs;
- parameter modes (`borrow` / `mut` / `own`) per function signature;
- which impl provides each trait method (`Program::impls`), so trait dispatch can be resolved at
  compile time instead of at run time as the interpreter does;
- a control-flow graph with move/borrow events for every function (`borrowck`), the raw material for
  drop elaboration (§7).

**Gaps to close before or during the backend** (details in §14):

1. Closure capture classification (by move / by mutable reference / by shared reference) is computed
   inside the borrow checker and not exported.
2. Per-function "which locals may be moved at scope exit" (drop flags) is implicit in the dataflow.
3. Recursive types are accepted by the checker but have no finite size (§8).
4. Two interpreter behaviours must change first so the backends can agree (§14).

---

## 6. Monomorphization and the IR

**Monomorphization.** Starting at `main` (or the `#[test]` functions), keep a worklist of
`(FnId, concrete type arguments)`. Each entry becomes one C function. When lowering a call, apply the
current instance's substitution to the call's recorded type arguments (the same `Ty::subst` the
interpreter uses at run time), look up the callee, and for a trait method pick the impl by matching
the concrete `Self` type (the same `match_ty` the interpreter uses). Default trait methods are
instantiated per implementing type. The result is a closed set of non-generic functions, named by a
mangled `(function, types)` string.

**Why an IR and not C straight from the AST.** Tau is expression-oriented (`if`, `match` and blocks
are expressions) and C is not; values have destructors; moves must be tracked. A small IR makes those
mechanical:

- **TIR**: per function, a list of basic blocks over **typed, monomorphic locals**. Instructions are
  three-address (`t3 = add_checked t1, t2`), calls, field/element access, loads/stores through
  places, `drop t`, `if/goto`. Every temporary is a named local with a concrete type.
- **No generics, no traits, no closures-as-values** left in TIR: calls are direct, closures are
  already structs plus a function (§10).
- The C emitter becomes a near-mechanical pretty-printer over TIR.

---

## 7. Ownership, drops and panics

Ownership is the reason we need no garbage collector.

- Every owning local is **dropped at scope exit** unless it was moved. `Vec`, `String`, `Map`, tensors
  and any struct containing them free their buffers.
- **Which locals are moved, and where**, is exactly what the borrow checker's dataflow computes. Drop
  elaboration reuses it: a local that is *definitely* moved on every path to scope exit needs no drop;
  one that is *never* moved is dropped unconditionally; one that is moved on only some paths gets a
  one-byte **drop flag**. (The same scheme Rust uses.)
- `own` parameters are dropped by the callee; `borrow`/`mut` parameters are never dropped by it.
- **Panics.** `tau_panic(msg)` prints `panic: msg` to stderr, flushes stdout, and exits with status 101
  (the interpreter's behaviour). Inside a `spawn`ed task a panic instead `longjmp`s to the task's
  entry point, which records the failure so `join()` can return `Err(TaskPanicked)`. Destructors do
  not run when a panic unwinds; this leaks memory but is safe, and the process normally exits anyway.
- **Stack overflow** is detected with a guard page and a `SIGSEGV` handler on an alternate stack that
  reports `panic: stack overflow` and exits 101, matching the interpreter.

---

## 8. Data representation

| Tau type | C representation |
|---|---|
| `i8`..`i64`, `u8`..`u64`, `isize`, `usize` | `int8_t`..`int64_t`, `uint8_t`..`uint64_t` (`isize`/`usize` are 64-bit) |
| `i128`, `u128` | `__int128`, `unsigned __int128` |
| `f32`, `f64` | `float`, `double` |
| `c64`, `c128` | `struct { float re, im; }`, `struct { double re, im; }` |
| `bool`, `char` | `uint8_t`, `uint32_t` (a Unicode scalar) |
| `()` and `Never` | no value (empty struct / `void`) |
| tuple, struct | a C `struct` of the fields (by-value copy is correct: `Copy` types copy, others are moved) |
| `[T; N]` | `struct { T v[N]; }` so arrays copy by value |
| enum | `struct { uint32_t tag; union { ... } }` |
| `Option<T>`, `Result<T,E>` | ordinary enums (niche optimisation: later, if profiling asks) |
| `str` | `struct { const uint8_t *ptr; size_t len; }` (a UTF-8 view) |
| `String` | `struct { uint8_t *ptr; size_t len, cap; }` (owned) |
| `Vec<T>` | `struct { T *ptr; size_t len, cap; }` |
| slice `[T]` | `struct { T *ptr; size_t len; }` (pointer + length, so a slice of a `Vec`, array or another slice is free) |
| `Map<K,V>` | runtime hash map, **insertion-ordered**, type-erased with per-type hash/equality/size callbacks |
| `Tensor<T, S>` | `struct { size_t rank, dims[8]; T *data; }`, column-major |
| `Range<T>` | `struct { T start, end; }` |
| `Ptr<T>` | `T *` |
| `fn(A) -> R`, closures | fat pointer `{ void *env; R (*call)(void *env, A) }` (§10) |
| `Task<T>` | pointer to a runtime task record holding a result slot and a join handle |

**Recursive types.** `enum List { Nil, Cons(i64, List) }` currently passes the checker but has
infinite size, so no backend can lay it out. This needs a language decision (§17, question 2): add a
`Box<T>` (owned heap pointer) to the prelude and make an unboxed recursive type a compile error.

---

## 9. Arithmetic, checks and parallel semantics

**Integers.** `+ - * / % ^` and unary `-` use the checked builtins and call `tau_overflow()` (exit 101,
message `integer overflow`); `/` and `%` also check for zero. Shifts check the amount. `as` between
integer types wraps (a C cast), float-to-int saturates with NaN to 0 (a small helper), exactly as in the
interpreter.

**Floats.** Plain IEEE operations, no reassociation, no FMA. `f32` arithmetic is done in `float`.

**Bounds.** Every `v[i]` and slice operation checks, with the interpreter's message
(`index out of bounds: the length is N but the index is I`).

**Formatting.** `print`, interpolation and `Display` need float printing that is the *shortest string that
reads back exactly* (the interpreter's behaviour). C has no such `printf`, so the runtime includes one
(try precisions 1 to 17 until `strtod` round-trips, or vendor Ryu). Whole floats keep the trailing `.0`.

**`par sum` / `par for` semantics (a language-level decision).** The iteration range is split into
fixed blocks of 4096 elements; each block is reduced sequentially from its first element; block results
are combined in block order. The answer therefore **does not depend on the number of threads**. The
interpreter is changed to evaluate in exactly this order so both backends agree to the last bit. For
`min`, `max`, `any` and `all` order does not matter. Integer `sum`/`prod` give the same value in any
order, except that a partial sum can overflow in one order and not another, so the blocked order is
what decides whether it panics. The only results whose printed digits change are float `sum` and
`prod`.

---

## 10. Closures and function values

A closure becomes a struct of its captures plus a C function taking a pointer to it. The borrow checker
already classifies each capture, which is exactly what the emitter needs:

- **by move**: the value is copied (or moved) into the struct;
- **by reference** (shared or mutable): a pointer to the captured variable.

By-reference captures point into the creator's stack frame. That is safe because the borrow checker's
escape rule already forbids a value that carries a borrow from outliving what it borrows, and a `spawn`
handle holds its loans until joined. A function item used as a value is wrapped in a trampoline so
`fn(A) -> R` has one representation. Direct calls to known functions and non-escaping closures are
inlined by `clang`.

---

## 11. Traits, operators and `Display`

All resolved at compile time. `a + b` on a user type becomes a direct call to the monomorphic `add`
instance; `==`, `<` call the `Eq`/`Ord` impls (with the same operand swapping the interpreter does).
`print(x)` and `"{x}"` generate, per type, a `tau_fmt_<type>(Buf *, const T *)` function: built-in
formatting for scalars, strings, tuples, arrays and tensors, and a call to the user's `Display::fmt`
for user types.

---

## 12. The runtime

One header-only C file embedded in the Tau binary (`include_str!`) and written out next to the
generated code, so there is no separate library to install. It provides:

- allocation helpers and `tau_panic` / `tau_overflow` / bounds helpers;
- `String`, `Vec`, insertion-ordered `Map`, and UTF-8 helpers (`lowercase`, `split_whitespace`);
- number formatting and `Buf` (a growable output buffer, flushed through one buffered `stdout`);
- file I/O (`fs.read/write/exists`, `io.read_line`) returning `Result<_, IoError>`;
- tensors: construction, element-wise ops, matrix product, `trace`, `det`, `inv`, Jacobi `eig`, FFT;
- the random generator (the same xorshift64* and seed, so `random` tensors match the interpreter);
- the thread pool and task machinery (§13).

---

## 13. Parallelism

- A **thread pool** of `TAU_THREADS` workers (default: the number of cores), created lazily on first use.
- **`par for i in lo..hi { body }`**: the body is compiled to `void body(void *env, size_t i)`; the
  range is divided into blocks and the pool runs them. No synchronisation is needed inside the body,
  because the borrow checker has already proved that each iteration writes only `out[i]` or its own
  locals and reads nothing another iteration writes.
- **`par for c in par_chunks(mut v, n)`**: each iteration gets its own slice, one chunk per block.
- **`par sum/prod/min/max/any/all`**: fixed-block reduction as defined in §9.
- **`spawn { ... }`**: runs on the pool, result stored in the task record; `join()` waits and returns
  `Ok(v)` or `Err(TaskPanicked)`. A handle dropped without `join` joins at scope exit, which is what the
  borrow checker already assumes.
- **`on`** runs locally, as in the interpreter.

Spike result: 3.9× on 4 cores for `par sum`.

---

## 14. Changes to the existing compiler, before the backend (milestone 0)

Small, each independently useful, each keeping the interpreter the reference.

| | Change | State |
|---|---|---|
| 1 | **Deterministic `par` reductions.** Float `sum`/`prod` (and every reduction) are evaluated in fixed 4096-item blocks combined in order (§9). The independent Python model gives `integrate` = `1.999999999998355` (was `...84077`) and Tau now matches it. | done |
| 2 | **Correct float printing.** An `f32` prints as the shortest text that round-trips *as an `f32`* (`0.1f32` shows `0.1`, not `0.10000000149011612`); whole floats of any size end in `.0`; never scientific notation. Printing is now type-directed, so `Vec<f32>`, tuples and tensors are right too. | done |
| 3 | **Reject infinite-size types.** `enum List { Nil, Cons(i64, List) }` (directly, through `Option`, a tuple, an array, a generic wrapper, or mutually) is an error with a hint to use a `Vec`. `Box<T>` is deferred until Tau has a way to look inside it. | done |
| 4 | **Export closure capture kinds** (move / mutable reference / shared reference) from the borrow checker. | moved to M3, designed against its first consumer |
| 5 | **Export per-local move/drop facts** for drop elaboration. | moved to M2, designed against its first consumer |
| 6 | **One shared "resolve this trait method for these types"** function used by the interpreter and the backend. | moved to M3 |

Items 4 to 6 are deliberately postponed: an interface designed before its first user is usually the
wrong shape, and each lands in the milestone that needs it.

---

## 15. Build and command line

- `tau build file.tau [-o out]`: check, compile, write `out`. `--emit-c` writes the C and stops.
- `tau run file.tau`: native when a C compiler is found, otherwise the interpreter, with one line saying
  which was used. `--interp` forces the interpreter (the reference).
- `tau test` uses the same choice.
- The compiler is invoked as `cc -O2 -ffp-contract=off -fno-strict-aliasing -pthread file.c -lm`; `-g`
  for debug builds; `-fsanitize=address,undefined` for the test suite's sanitizer run.
- Generated C and the binary are cached by a hash of the source, so re-running an unchanged program is
  instant.

Target: `hello` compiles in under half a second.

---

## 16. Testing strategy

The interpreter is the oracle. We already own the oracle's corpus:

1. **Differential tests.** Every program we have, run on both backends: the 71 snippets in
   `tests/run.rs`, the programs in `examples/run/`, and `examples/checked/`. Stdout and exit status
   must be byte-identical. The existing test helpers take a `Backend` parameter, so a feature is "done"
   when its existing tests pass on both.
2. **Independent oracle.** The seven programs with Python reference outputs must still match.
3. **Mutation fuzzing, differentially.** The existing line-mutation fuzzer produces hundreds of
   programs that pass the checkers; run each on both backends and compare (including panic messages and
   exit codes).
4. **Sanitizers.** The same corpus under `-fsanitize=address,undefined`: any UB, leak or double free in
   generated code fails the build. This is how ownership bugs in drop elaboration are found.
5. **Benchmarks as tests.** The programs in §2 with generous time ceilings, to catch accidental
   slowdowns.

---

## 17. Milestones

Each milestone ends with: the listed programs passing differentially, sanitizers clean.

| # | Scope | Done when |
|---|---|---|
| **M0** | Prerequisites (§14, items 1 to 3) | **done**: interpreter and checker changes, with tests |
| **M1** | Scalars, tuples, functions, control flow, checked arithmetic, panics, `print` of scalars/strings/tuples | **done**: `hello`, `fizzbuzz`, `collatz`, 36 differential tests (`tests/native.rs`) including a 10,000-value float-printing fuzz |
| **M2** | Structs, enums, `Option`/`Result`/`?`, `match` on them, arrays, `Vec`, slices, `String`, `str`, `Map`, ownership and drops (incl. §14 item 5) | `primes`, `sort`, `bank`, `wordcount`, `errors`, `shapes`, `ownership` |
| **M3** | Generics, traits, default methods, operators, `Display`/`Eq`/`Ord`, closures, `fn` values (incl. §14 items 4 and 6) | `nbody`, `traits`, `borrowing`, closure tests |
| **M4** | Tensors, maths library, `linalg`, FFT | `tensors`, matrix and FFT tests |
| **M5** | Thread pool, `par for`/`par sum`/`par_chunks`, `spawn`/`join`, task panics | `parallel`, `integrate`; ≥3.5× on 4 cores |
| **M6** | `tau build`/`run`/`test` integration, caching, error messages, docs, benchmark suite | all of the above, native by default |

### What M1 changed in the plan

- **No TIR yet.** The generator emits C straight from the checked AST, using GNU statement expressions
  (`({ ... })`) so Tau's expression-oriented blocks, `if`, `match` and `loop ... break value` map directly,
  and `goto` for `break`/`continue`. This is enough while everything is monomorphic and nothing needs
  dropping; the TIR in §6 arrives with generics (M3) or drop elaboration (M2), whichever needs it first.
- **Anything unsupported is reported, never miscompiled.** The generator returns `Unsupported("... milestone N")`
  and `tau run` falls back to the interpreter (`-v` says why; `--native` makes it an error).
- **Flags that matter for matching the interpreter**: `-ffp-contract=off` (no fused multiply-add),
  `-fno-optimize-sibling-calls` (runaway recursion must overflow the stack and panic, not become a loop),
  `-fwrapv`. Verified by `runaway_recursion_is_a_stack_overflow_panic`.
- **Two interpreter/checker bugs found by the differential tests, fixed**: integer literals that do not fit
  their type were silently accepted (`let a: i8 = 200`); they are now a compile error, with `-128i8` still
  fine. `abs` of the most negative integer did not overflow.
- **Float printing** needed more than "shortest `%.*e` that round-trips": at a power of two the interval of
  values reading back as `x` is narrower below, so the right digits can be the *neighbour* of the correctly
  rounded ones; and an exact tie must round up (Rust) where `printf` rounds to even. Both found by the fuzz.
- **Sanitizers**: `TAU_CC=gcc TAU_SANITIZE=1 cargo test --test native` runs the whole differential suite
  under UBSan and aborts on the first report (it found a stack buffer overflow in the float printer).
  AddressSanitizer needs the runtime's 1 GB stack thread and alternate signal stack to be switched off in
  sanitizer builds; planned for M2, when there is heap ownership worth checking.
- **Known, accepted differences**: the native stack is 1 GB, so very deep (but finite) recursion that makes
  the interpreter report a stack overflow still succeeds natively.

M1 to M3 are the bulk of the work; M4 and M5 are mostly runtime code; M6 is polish.

---

## 18. Risks

| Risk | Mitigation |
|---|---|
| Drop elaboration bugs (leaks, double frees) | built from the borrow checker's own dataflow; the sanitizer run on the whole corpus |
| C undefined behaviour in generated code | checked builtins everywhere; UBSan in CI; `-fno-strict-aliasing` |
| Float results differ between backends | `-ffp-contract=off`, no fast-math, same operation order; deterministic `par sum`; differential tests compare printed digits |
| Compile time grows with monomorphization | measure early; only instantiate what is reachable from `main`; cache by source hash |
| Users lack a C compiler | detect and fall back to the interpreter with a clear message; LLVM emitter is the later answer |
| Type-erased `Map` is slower than a specialised one | acceptable for v1; specialise hot key types (`i64`, `str`) later |
| Scope creep into optimisation | the target is "same behaviour, ~100× faster", which clang already delivers |

---

## 19. Decisions

Answered:

1. **A system C compiler is required for native builds.** Yes. The interpreter remains the fallback when none is found.
2. **Recursive types.** Rejected with a clear error for now; `Box<T>` comes later, together with a way to
   look inside it (a dereference syntax or pattern), which deserves its own design.
3. **Deterministic `par` reductions.** Yes: fixed 4096-item blocks combined in order, so a result never
   depends on the number of cores. (Recommended and accepted; costs nothing measurable, see §2.)
4. **`tau run` defaults to native** when a C compiler is available; `--interp` selects the reference interpreter.

Still open (not blocking M1 to M3):

5. **Platforms.** Linux first (what can be tested here). macOS/Windows later; threads and the
   stack-overflow handler are the platform-specific parts.
6. **BLAS.** Our own kernels reproduce the interpreter bit for bit. Linking the system BLAS would speed up
   large matrix products but change the last digits; opt-in later (`--blas`), or never?
