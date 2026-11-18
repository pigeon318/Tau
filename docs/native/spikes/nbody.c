// Spike: examples/run/nbody.tau with `Vec3` operators as inline functions.
//   cc -O2 nbody.c -lm -o nbody
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
typedef struct { double x, y, z; } Vec3;
typedef struct { Vec3 pos, vel; double mass; } Body;
static inline Vec3 v_add(Vec3 a, Vec3 b) { return (Vec3){a.x + b.x, a.y + b.y, a.z + b.z}; }
static inline Vec3 v_sub(Vec3 a, Vec3 b) { return (Vec3){a.x - b.x, a.y - b.y, a.z - b.z}; }
static inline Vec3 v_mul(Vec3 a, double k) { return (Vec3){a.x * k, a.y * k, a.z * k}; }
static inline double v_dot(Vec3 a, Vec3 b) { return a.x * b.x + a.y * b.y + a.z * b.z; }
static inline double v_norm(Vec3 a) { return sqrt(v_dot(a, a)); }
typedef struct { Body *ptr; size_t len; } BodySlice;   // a Tau slice: pointer + length
static void tau_panic(const char *m) { fprintf(stderr, "panic: %s\n", m); exit(101); }
#define IDX(s, i) (((size_t)(i) < (s).len) ? &(s).ptr[(size_t)(i)] : (tau_panic("index out of bounds"), (Body *)0))
static void step(BodySlice bodies, double dt) {
    for (size_t i = 0; i < bodies.len; i++) {
        Vec3 acc = {0, 0, 0};
        for (size_t j = 0; j < bodies.len; j++) {
            if (!(j != i)) continue;
            Vec3 d = v_sub(IDX(bodies, j)->pos, IDX(bodies, i)->pos);
            acc = v_add(acc, v_mul(d, 6.674e-11 * IDX(bodies, j)->mass / pow(v_norm(d), 3.0)));
        }
        IDX(bodies, i)->vel = v_add(IDX(bodies, i)->vel, v_mul(acc, dt));
    }
    for (size_t k = 0; k < bodies.len; k++) { Body *b = IDX(bodies, k); b->pos = v_add(b->pos, v_mul(b->vel, dt)); }
}
static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec * 1e-9; }
int main(int argc, char **argv) {
    long steps = argc > 1 ? atol(argv[1]) : 1000;
    Body arr[2] = {{{0, 0, 0}, {0, 0, 0}, 1.0e10}, {{1, 0, 0}, {0, 1, 0}, 1.0}};
    BodySlice bodies = {arr, 2};
    double t0 = now();
    for (long t = 0; t < steps; t++) step(bodies, 0.01);
    double e = 0.0;
    for (size_t i = 0; i < 2; i++) e += 0.5 * arr[i].mass * v_dot(arr[i].vel, arr[i].vel);
    printf("steps=%ld energy = %.17g  %.3fs\n", steps, e, now() - t0);
    return 0;
}
