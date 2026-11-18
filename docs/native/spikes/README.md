# Native backend spikes

Hand-written C, shaped like what the compiler would generate for programs in `examples/run/`,
including Tau's safety checks. They exist to ground [`../DESIGN.md`](../DESIGN.md) in measurements.
They are not part of the build.

```bash
clang -O2 integrate.c -lm -lpthread -o /tmp/integrate && /tmp/integrate
clang -O2 nbody.c -lm -o /tmp/nbody && /tmp/nbody 100000
clang -O2 collatz.c -o /tmp/collatz && /tmp/collatz 100000
```

| File | What it demonstrates |
|---|---|
| `integrate.c` | `par sum` as a fixed-block reduction: identical result for 1, 2 and 4 threads, 3.9× scaling on 4 cores; the sequential result matches the interpreter digit for digit |
| `nbody.c` | structs and operator functions as inline C; bounds-checked slice indexing; prints the same energy as the interpreter after 100,000 steps |
| `collatz.c` | overflow-checked integer arithmetic costs about 1.7× in the tightest loop |

Measured on a 4-core machine with clang 18 (`-O2`); interpreter figures are from `cargo build --release`.

| Program | Interpreter | Native | Speed-up |
|---|---|---|---|
| integrate, 10M steps | 4.55 s | 0.054 s (1 thread), 0.018 s (4 threads) | 84×, 250× |
| collatz, to 100,000 | 4.12 s | 0.022 s | 187× |
| nbody, 100,000 steps | 1.42 s | 0.007 s | 200× |
