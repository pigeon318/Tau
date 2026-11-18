// Spike: what the compiler would emit for examples/run/integrate.tau, plus a
// deterministic parallel version of `par sum`.
//   cc -O2 integrate.c -lm -lpthread -o integrate && ./integrate
#include <math.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <time.h>

typedef double (*fn_f64_f64)(double);
static double tau_sin(double x) { return sin(x); }

// sequential `par sum(i in 1..n) f(a + i as f64 * h)`
static double sum_seq(fn_f64_f64 f, double a, double h, uint64_t n) {
    double s = 0.0; int first = 1;
    for (uint64_t i = 1; i < n; i++) { double v = f(a + (double)i * h); s = first ? v : s + v; first = 0; }
    return s;
}

// Parallel: fixed-size blocks, each summed in order, block results combined in order.
// The answer does not depend on the number of threads.
#define BLOCK 4096
typedef struct { fn_f64_f64 f; double a, h; uint64_t lo, hi; double *out; uint64_t nblocks; int tid, nthreads; } job;
static void *worker(void *p) {
    job *j = p;
    for (uint64_t b = j->tid; b < j->nblocks; b += j->nthreads) {
        uint64_t lo = j->lo + b * BLOCK, hi = lo + BLOCK < j->hi ? lo + BLOCK : j->hi;
        double s = 0.0; int first = 1;
        for (uint64_t i = lo; i < hi; i++) { double v = j->f(j->a + (double)i * j->h); s = first ? v : s + v; first = 0; }
        j->out[b] = s;
    }
    return 0;
}
static double sum_par(fn_f64_f64 f, double a, double h, uint64_t n, int nthreads, double *scratch) {
    uint64_t total = n - 1, nblocks = (total + BLOCK - 1) / BLOCK;
    pthread_t th[64]; job jobs[64];
    for (int t = 0; t < nthreads; t++) {
        jobs[t] = (job){f, a, h, 1, n, scratch, nblocks, t, nthreads};
        pthread_create(&th[t], 0, worker, &jobs[t]);
    }
    for (int t = 0; t < nthreads; t++) pthread_join(th[t], 0);
    double s = scratch[0];
    for (uint64_t b = 1; b < nblocks; b++) s += scratch[b];
    return s;
}
static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec * 1e-9; }

int main(void) {
    const uint64_t n = 10000000; const double a = 0.0, b = M_PI, h = (b - a) / (double)n;
    static double scratch[1 << 20];
    double t0 = now(); double s = sum_seq(tau_sin, a, h, n); double r = h * (s + 0.5 * (tau_sin(a) + tau_sin(b)));
    printf("sequential: %.17g  %.3fs\n", r, now() - t0);
    for (int th = 1; th <= 4; th *= 2) {
        t0 = now(); s = sum_par(tau_sin, a, h, n, th, scratch); r = h * (s + 0.5 * (tau_sin(a) + tau_sin(b)));
        printf("parallel x%d: %.17g  %.3fs\n", th, r, now() - t0);
    }
    return 0;
}
