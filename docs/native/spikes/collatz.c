// Spike: integer code with Tau's overflow checking, vs. unchecked arithmetic.
//   cc -O2 collatz.c -o collatz
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
static void tau_overflow(void) { fprintf(stderr, "panic: integer overflow\n"); exit(101); }
static inline int64_t add(int64_t a, int64_t b) { int64_t r; if (__builtin_add_overflow(a, b, &r)) tau_overflow(); return r; }
static inline int64_t mul(int64_t a, int64_t b) { int64_t r; if (__builtin_mul_overflow(a, b, &r)) tau_overflow(); return r; }
static int64_t steps_checked(int64_t start) {
    int64_t n = start, count = 0;
    while (n != 1) { if (n % 2 == 0) n = n / 2; else n = add(mul(3, n), 1); count = add(count, 1); }
    return count;
}
static int64_t steps_unchecked(int64_t start) {
    int64_t n = start, count = 0;
    while (n != 1) { if (n % 2 == 0) n = n / 2; else n = 3 * n + 1; count++; }
    return count;
}
static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec * 1e-9; }
int main(int argc, char **argv) {
    int64_t limit = argc > 1 ? atol(argv[1]) : 1000;
    for (int pass = 0; pass < 2; pass++) {
        double t0 = now(); int64_t best = 1, best_steps = 0;
        for (int64_t n = 1; n < limit; n++) { int64_t s = pass ? steps_unchecked(n) : steps_checked(n); if (s > best_steps) { best = n; best_steps = s; } }
        printf("%-9s longest chain under %ld: %ld with %ld steps  %.3fs\n", pass ? "unchecked" : "checked", limit, best, best_steps, now() - t0);
    }
    return 0;
}
