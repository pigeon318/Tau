/* ===== Tau runtime (milestone 1: scalars, strings, tuples, panics, output) =====
 *
 * Everything here mirrors the interpreter: panic messages, number formatting and
 * integer overflow behaviour must match it exactly (see tests/native.rs).
 */
#define _GNU_SOURCE
#include <math.h>
#include <pthread.h>
#include <signal.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

typedef unsigned char tau_unit;
typedef struct { const uint8_t *ptr; size_t len; } tau_str;
typedef struct { uint8_t *ptr; size_t len, cap; } tau_buf;

#define TAU_NORETURN __attribute__((noreturn))
#define TAU_COLD __attribute__((cold, noinline))
#define TAU_UNLIKELY(x) __builtin_expect(!!(x), 0)

/* ---------------------------------------------------------------- buffers and output */

static void tau_buf_push(tau_buf *b, const void *p, size_t n) {
    if (n == 0) return;
    if (b->len + n > b->cap) {
        size_t c = b->cap ? b->cap * 2 : 256;
        while (c < b->len + n) c *= 2;
        b->ptr = (uint8_t *)realloc(b->ptr, c);
        if (!b->ptr) { static const char m[] = "panic: out of memory\n"; write(2, m, sizeof m - 1); _exit(101); }
        b->cap = c;
    }
    memcpy(b->ptr + b->len, p, n);
    b->len += n;
}
static void tau_buf_cstr(tau_buf *b, const char *s) { tau_buf_push(b, s, strlen(s)); }
static void tau_buf_char(tau_buf *b, char c) { tau_buf_push(b, &c, 1); }

static tau_buf tau_stdout;

static void tau_flush_stdout(void) {
    size_t off = 0;
    while (off < tau_stdout.len) {
        ssize_t w = write(1, tau_stdout.ptr + off, tau_stdout.len - off);
        if (w <= 0) break;
        off += (size_t)w;
    }
    tau_stdout.len = 0;
}

/* `print` writes through this buffer, which is flushed when it grows and at exit/panic. */
static tau_buf *tau_out_begin(void) { return &tau_stdout; }
static void tau_out_end(void) { if (tau_stdout.len >= (1u << 16)) tau_flush_stdout(); }

/* ---------------------------------------------------------------- panics */

TAU_NORETURN static void tau_panic(const char *msg, size_t len) {
    tau_flush_stdout();
    static const char pre[] = "panic: ";
    (void)!write(2, pre, sizeof pre - 1);
    (void)!write(2, msg, len);
    (void)!write(2, "\n", 1);
    _exit(101);
}
TAU_NORETURN static void tau_panic_cstr(const char *m) { tau_panic(m, strlen(m)); }
TAU_NORETURN static void tau_panic_str(tau_str s) { tau_panic((const char *)s.ptr, s.len); }

/* ---------------------------------------------------------------- integer formatting */

static size_t tau_u128_to_str(char *out, unsigned __int128 v) {
    char tmp[48];
    size_t n = 0;
    if (v == 0) tmp[n++] = '0';
    while (v) { tmp[n++] = (char)('0' + (int)(v % 10)); v /= 10; }
    for (size_t i = 0; i < n; i++) out[i] = tmp[n - 1 - i];
    return n;
}
static size_t tau_i128_to_str(char *out, __int128 v) {
    if (v < 0) { out[0] = '-'; return 1 + tau_u128_to_str(out + 1, (unsigned __int128)0 - (unsigned __int128)v); }
    return tau_u128_to_str(out, (unsigned __int128)v);
}
static void tau_put_i128(tau_buf *b, __int128 v) { char t[48]; tau_buf_push(b, t, tau_i128_to_str(t, v)); }
static void tau_put_u128(tau_buf *b, unsigned __int128 v) { char t[48]; tau_buf_push(b, t, tau_u128_to_str(t, v)); }

/* ---------------------------------------------------------------- checked integer arithmetic */

TAU_COLD TAU_NORETURN static void tau_ovf_wide(const char *ty, __int128 v) {
    char m[200], num[48];
    num[tau_i128_to_str(num, v)] = 0;
    snprintf(m, sizeof m, "integer overflow: %s does not fit in %s", num, ty);
    tau_panic_cstr(m);
}
TAU_COLD TAU_NORETURN static void tau_ovf_op(const char *ty, char op) {
    char m[120];
    snprintf(m, sizeof m, "integer overflow in `%c` on %s", op, ty);
    tau_panic_cstr(m);
}
/* The slow path of + - *: recompute exactly (in 128 bits) to say what went wrong. */
TAU_COLD TAU_NORETURN static void tau_ovf_arith(const char *ty, char op, __int128 a, __int128 b, __int128 minv, __int128 maxv, int is128) {
    __int128 w;
    int o = op == '+' ? __builtin_add_overflow(a, b, &w) : op == '-' ? __builtin_sub_overflow(a, b, &w) : __builtin_mul_overflow(a, b, &w);
    if (o || is128 || w < minv || w > maxv) {
        if (o || is128) tau_ovf_op(ty, op);
    }
    tau_ovf_wide(ty, w);
}
TAU_COLD TAU_NORETURN static void tau_shift_panic(const char *ty, __int128 amount) {
    char m[160], num[48];
    num[tau_i128_to_str(num, amount)] = 0;
    snprintf(m, sizeof m, "shift amount %s is too large for %s", num, ty);
    tau_panic_cstr(m);
}

/* N = Tau name, T = C type, UT = unsigned twin, MINV/MAXV as __int128, IS128 = full 128 bits. */
#define TAU_INT(N, T, UT, MINV, MAXV, IS128, BITS)                                                              \
    static inline T tau_add_##N(T a, T b) { T r; if (TAU_UNLIKELY(__builtin_add_overflow(a, b, &r))) tau_ovf_arith(#N, '+', (__int128)a, (__int128)b, MINV, MAXV, IS128); return r; } \
    static inline T tau_sub_##N(T a, T b) { T r; if (TAU_UNLIKELY(__builtin_sub_overflow(a, b, &r))) tau_ovf_arith(#N, '-', (__int128)a, (__int128)b, MINV, MAXV, IS128); return r; } \
    static inline T tau_mul_##N(T a, T b) { T r; if (TAU_UNLIKELY(__builtin_mul_overflow(a, b, &r))) tau_ovf_arith(#N, '*', (__int128)a, (__int128)b, MINV, MAXV, IS128); return r; } \
    static inline T tau_div_##N(T a, T b) {                                                                    \
        if (TAU_UNLIKELY(b == 0)) tau_panic_cstr("division by zero");                                          \
        if (TAU_UNLIKELY((T)-1 < 0 && b == (T)-1 && a == (T)(MINV))) { if (IS128) tau_ovf_op(#N, '/'); tau_ovf_wide(#N, -(__int128)(MINV)); } \
        return a / b; }                                                                                         \
    static inline T tau_rem_##N(T a, T b) {                                                                    \
        if (TAU_UNLIKELY(b == 0)) tau_panic_cstr("division by zero");                                          \
        if ((T)-1 < 0 && b == (T)-1) return 0;                                                                 \
        return a % b; }                                                                                         \
    static inline T tau_neg_##N(T a) {                                                                         \
        if ((T)-1 < 0) { if (TAU_UNLIKELY(a == (T)(MINV))) { if (IS128) tau_ovf_op(#N, '-'); tau_ovf_wide(#N, -(__int128)a); } return (T)(0 - a); } \
        if (TAU_UNLIKELY(a != 0)) tau_ovf_wide(#N, -(__int128)a);                                              \
        return 0; }                                                                                             \
    static inline T tau_pow_##N(T a, T e) {                                                                    \
        if ((T)-1 < 0 && e < 0) tau_panic_cstr("integer power with a negative exponent");                      \
        if (e == 0) return 1;                                                                                   \
        __int128 base = (__int128)a, acc = 1;                                                                   \
        unsigned __int128 ex = (unsigned __int128)e;                                                            \
        for (;;) {                                                                                              \
            if (ex & 1) { if (__builtin_mul_overflow(acc, base, &acc)) tau_ovf_op(#N, '^'); if (ex == 1) break; } \
            ex /= 2;                                                                                            \
            if (__builtin_mul_overflow(base, base, &base)) tau_ovf_op(#N, '^');                                 \
        }                                                                                                       \
        if (!(IS128) && (acc < (MINV) || acc > (MAXV))) tau_ovf_wide(#N, acc);                                  \
        return (T)acc; }                                                                                        \
    static inline T tau_shl_##N(T a, T b) {                                                                    \
        if (TAU_UNLIKELY(((T)-1 < 0 && b < 0) || (unsigned __int128)b >= (BITS))) tau_shift_panic(#N, (__int128)b); \
        return (T)((UT)a << b); }                                                                               \
    static inline T tau_shr_##N(T a, T b) {                                                                    \
        if (TAU_UNLIKELY(((T)-1 < 0 && b < 0) || (unsigned __int128)b >= (BITS))) tau_shift_panic(#N, (__int128)b); \
        return (T)(a >> b); }                                                                                   \
    static inline double tau_f64_from_##N(T a) { return (double)a; }                                           \
    static inline T tau_from_f64_##N(double x) {                                                               \
        if (x != x) return 0;                                                                                   \
        if (x >= (double)(MAXV)) return (T)(MAXV);                                                              \
        if (x <= (double)(MINV)) return (T)(MINV);                                                              \
        return (T)x; }                                                                                          \
    static void tau_fmt_##N(tau_buf *b, T v) { if ((T)-1 < 0) tau_put_i128(b, (__int128)v); else tau_put_u128(b, (unsigned __int128)v); }

#define TAU_I128_MAX ((__int128)(((unsigned __int128)1 << 127) - 1))
#define TAU_I128_MIN (-TAU_I128_MAX - 1)
TAU_INT(i8, int8_t, uint8_t, INT8_MIN, INT8_MAX, 0, 8)
TAU_INT(i16, int16_t, uint16_t, INT16_MIN, INT16_MAX, 0, 16)
TAU_INT(i32, int32_t, uint32_t, INT32_MIN, INT32_MAX, 0, 32)
TAU_INT(i64, int64_t, uint64_t, INT64_MIN, INT64_MAX, 0, 64)
TAU_INT(isize, int64_t, uint64_t, INT64_MIN, INT64_MAX, 0, 64)
TAU_INT(i128, __int128, unsigned __int128, TAU_I128_MIN, TAU_I128_MAX, 1, 128)
TAU_INT(u8, uint8_t, uint8_t, 0, UINT8_MAX, 0, 8)
TAU_INT(u16, uint16_t, uint16_t, 0, UINT16_MAX, 0, 16)
TAU_INT(u32, uint32_t, uint32_t, 0, UINT32_MAX, 0, 32)
TAU_INT(u64, uint64_t, uint64_t, 0, UINT64_MAX, 0, 64)
TAU_INT(usize, uint64_t, uint64_t, 0, UINT64_MAX, 0, 64)
TAU_INT(u128, unsigned __int128, unsigned __int128, 0, TAU_I128_MAX, 0, 128)

/* ---------------------------------------------------------------- strings, chars, bools */

static inline bool tau_str_eq(tau_str a, tau_str b) { return a.len == b.len && memcmp(a.ptr, b.ptr, a.len) == 0; }
static inline int tau_str_cmp(tau_str a, tau_str b) {
    size_t n = a.len < b.len ? a.len : b.len;
    int c = memcmp(a.ptr, b.ptr, n);
    if (c) return c < 0 ? -1 : 1;
    return a.len < b.len ? -1 : a.len > b.len ? 1 : 0;
}

static void tau_fmt_char(tau_buf *b, uint32_t c) {
    uint8_t u[4];
    size_t n;
    if (c < 0x80) { u[0] = (uint8_t)c; n = 1; }
    else if (c < 0x800) { u[0] = (uint8_t)(0xC0 | (c >> 6)); u[1] = (uint8_t)(0x80 | (c & 0x3F)); n = 2; }
    else if (c < 0x10000) { u[0] = (uint8_t)(0xE0 | (c >> 12)); u[1] = (uint8_t)(0x80 | ((c >> 6) & 0x3F)); u[2] = (uint8_t)(0x80 | (c & 0x3F)); n = 3; }
    else { u[0] = (uint8_t)(0xF0 | (c >> 18)); u[1] = (uint8_t)(0x80 | ((c >> 12) & 0x3F)); u[2] = (uint8_t)(0x80 | ((c >> 6) & 0x3F)); u[3] = (uint8_t)(0x80 | (c & 0x3F)); n = 4; }
    tau_buf_push(b, u, n);
}

/* ---------------------------------------------------------------- sequences and strings */

TAU_NORETURN TAU_COLD static void tau_oob(size_t len, __int128 idx) {
    tau_buf m = {0};
    tau_buf_cstr(&m, "index out of bounds: the length is ");
    tau_fmt_u64(&m, (uint64_t)len);
    tau_buf_cstr(&m, " but the index is ");
    tau_fmt_i128(&m, idx);
    tau_panic((const char *)m.ptr, m.len);
}

TAU_NORETURN TAU_COLD static void tau_slice_oob(__int128 a, __int128 b, size_t len) {
    tau_buf m = {0};
    tau_buf_cstr(&m, "slice ");
    tau_fmt_i128(&m, a);
    tau_buf_cstr(&m, "..");
    tau_fmt_i128(&m, b);
    tau_buf_cstr(&m, " is out of range for length ");
    tau_fmt_u64(&m, (uint64_t)len);
    tau_panic((const char *)m.ptr, m.len);
}

/* A string built by interpolation (`let s = "n = {n}"`) has type `str`, a view that cannot own
 * memory, so its bytes live until the program ends. They are kept on a list so that they stay
 * reachable (and a leak checker sees them as such). */
static void *tau_pool_head;
static tau_str tau_intern(const uint8_t *p, size_t n) {
    void **blk = (void **)malloc(n + sizeof(void *));
    if (!blk) tau_panic_cstr("out of memory");
    *blk = tau_pool_head;
    tau_pool_head = blk;
    if (n) memcpy(blk + 1, p, n);
    return (tau_str){ (const uint8_t *)(blk + 1), n };
}

/* hashing for map keys */
static uint64_t tau_hash_u64(uint64_t x) {
    x += 0x9e3779b97f4a7c15ULL;
    x = (x ^ (x >> 30)) * 0xbf58476d1ce4e5b9ULL;
    x = (x ^ (x >> 27)) * 0x94d049bb133111ebULL;
    return x ^ (x >> 31);
}
static uint64_t tau_hash_bytes(const uint8_t *p, size_t n) {
    uint64_t h = 0xcbf29ce484222325ULL;
    for (size_t i = 0; i < n; i++) { h ^= p[i]; h *= 0x100000001b3ULL; }
    return tau_hash_u64(h);
}
static uint64_t tau_hash_mix(uint64_t a, uint64_t b) { return tau_hash_u64(a * 31 + b); }

static int tau_is_space(uint8_t c) { return c == ' ' || (c >= 9 && c <= 13); }

/* ASCII case mapping (the interpreter maps all of Unicode; see docs/native/DESIGN.md) */
static tau_buf tau_str_case(tau_str s, int upper) {
    tau_buf b = {0};
    if (s.len == 0) return b;
    tau_buf_push(&b, s.ptr, s.len);
    for (size_t i = 0; i < b.len; i++) {
        uint8_t c = b.ptr[i];
        if (upper) { if (c >= 'a' && c <= 'z') b.ptr[i] = (uint8_t)(c - 32); }
        else { if (c >= 'A' && c <= 'Z') b.ptr[i] = (uint8_t)(c + 32); }
    }
    return b;
}

TAU_NORETURN TAU_COLD static void tau_bad_char(__int128 v) {
    tau_buf m = {0};
    tau_fmt_i128(&m, v);
    tau_buf_cstr(&m, " is not a valid character");
    tau_panic((const char *)m.ptr, m.len);
}
static uint32_t tau_char_from_int(__int128 v) {
    uint32_t c = (uint32_t)v;
    if (TAU_UNLIKELY(c >= 0x110000u || (c >= 0xD800u && c < 0xE000u))) tau_bad_char(v);
    return c;
}
static void tau_fmt_bool(tau_buf *b, bool v) { tau_buf_cstr(b, v ? "true" : "false"); }
static void tau_fmt_unit(tau_buf *b, tau_unit v) { (void)v; tau_buf_cstr(b, "()"); }
static void tau_fmt_str(tau_buf *b, tau_str s) { tau_buf_push(b, s.ptr, s.len); }

/* ---------------------------------------------------------------- floats */

/* The shortest digits that read back exactly (as a float when `single`), never in
 * scientific notation, with ".0" added to a whole number. Same rule as the interpreter. */
__attribute__((noinline)) static void tau_fmt_float(tau_buf *b, double x, int single) {
    if (x != x) { tau_buf_cstr(b, "NaN"); return; }
    if (isinf(x)) { tau_buf_cstr(b, x > 0 ? "inf" : "-inf"); return; }
    if (x == 0) { tau_buf_cstr(b, signbit(x) ? "-0.0" : "0.0"); return; }
    if (signbit(x)) { tau_buf_char(b, '-'); x = -x; }
    /* For each digit count p, try the correctly rounded p digits and then its neighbours:
     * at a power of two the interval of values that read back as x is narrower below than
     * above, so a neighbour can round-trip when the nearest p-digit decimal does not. */
    char buf[64], digits[32];
    int p, e10 = 0, nd = 0, maxp = single ? 9 : 17;
    unsigned long long m = 0;
    int q = 0;
    for (p = 1; p <= maxp; p++) {
        snprintf(buf, sizeof buf, "%.*e", p - 1, x);
        char *e = strchr(buf, 'e');
        int de = atoi(e + 1), k = 0;
        char ds[32];
        for (char *c = buf; c < e; c++) if (*c != '.') ds[k++] = *c;
        ds[k] = 0;
        m = strtoull(ds, 0, 10);
        q = de - (p - 1);
        if (single ? (strtof(buf, 0) == (float)x) : (strtod(buf, 0) == x)) {
            /* printf breaks an exact tie to even; the interpreter (Rust) rounds it up when both read back */
            char ex[96], t[64];
            snprintf(ex, sizeof ex, "%.50e", x);
            int n = 0, tie = 0;
            for (char *c = ex; *c && *c != 'e'; c++) if (*c != '.') t[n++] = *c;
            if (n > p && t[p] == '5') {
                tie = 1;
                for (int i = p + 1; i < n; i++) if (t[i] != '0') tie = 0;
            }
            if (tie) {
                t[p] = 0;
                unsigned long long up = strtoull(t, 0, 10) + 1;
                snprintf(buf, sizeof buf, "%llue%d", up, q);
                if (single ? (strtof(buf, 0) == (float)x) : (strtod(buf, 0) == x)) m = up;
            }
            break;
        }
        int found = 0;
        for (int d = 1; d >= -1 && !found; d -= 2) {
            if (d < 0 && m <= 1) break;
            snprintf(buf, sizeof buf, "%llue%d", m + (unsigned long long)(long long)d, q);
            if (single ? (strtof(buf, 0) == (float)x) : (strtod(buf, 0) == x)) { m += (unsigned long long)(long long)d; found = 1; }
        }
        if (found) break;
    }
    {
        int L = snprintf(digits, sizeof digits, "%llu", m);
        e10 = L - 1 + q;
        nd = L;
        while (nd > 1 && digits[nd - 1] == '0') nd--;
    }
    if (e10 + 1 >= nd) {                       /* a whole number */
        tau_buf_push(b, digits, (size_t)nd);
        for (int i = 0; i < e10 + 1 - nd; i++) tau_buf_char(b, '0');
        tau_buf_cstr(b, ".0");
    } else if (e10 >= 0) {
        tau_buf_push(b, digits, (size_t)(e10 + 1));
        tau_buf_char(b, '.');
        tau_buf_push(b, digits + e10 + 1, (size_t)(nd - e10 - 1));
    } else {
        tau_buf_cstr(b, "0.");
        for (int i = 0; i < -e10 - 1; i++) tau_buf_char(b, '0');
        tau_buf_push(b, digits, (size_t)nd);
    }
}
static void tau_fmt_f64(tau_buf *b, double x) { tau_fmt_float(b, x, 0); }
static void tau_fmt_f32(tau_buf *b, float x) { tau_fmt_float(b, (double)x, 1); }

/* ---------------------------------------------------------------- complex numbers */

/* c64 and c128 are both pairs of doubles (the interpreter computes c64 in double too); they
 * differ only in how they print. */
typedef struct { double re, im; } tau_c128;
typedef struct { double re, im; } tau_c64;

#define TAU_CPLX(T)                                                                                         \
    static T T##_add(T a, T b) { return (T){ a.re + b.re, a.im + b.im }; }                                  \
    static T T##_sub(T a, T b) { return (T){ a.re - b.re, a.im - b.im }; }                                  \
    static T T##_mul(T a, T b) { return (T){ a.re * b.re - a.im * b.im, a.re * b.im + a.im * b.re }; }      \
    static T T##_div(T a, T b) {                                                                            \
        double den = b.re * b.re + b.im * b.im;                                                             \
        return (T){ (a.re * b.re + a.im * b.im) / den, (a.im * b.re - a.re * b.im) / den };                 \
    }                                                                                                       \
    static T T##_neg(T a) { return (T){ -a.re, -a.im }; }                                                   \
    static T T##_abs(T a) { return (T){ hypot(a.re, a.im), 0.0 }; }                                         \
    static bool T##_eq(T a, T b) { return a.re == b.re && a.im == b.im; }
TAU_CPLX(tau_c128)
TAU_CPLX(tau_c64)

static void tau_fmt_c128(tau_buf *b, tau_c128 z) {
    if (z.im < 0 || (z.im == 0 && signbit(z.im))) { tau_fmt_f64(b, z.re); tau_buf_char(b, '-'); tau_fmt_f64(b, -z.im); }
    else { tau_fmt_f64(b, z.re); tau_buf_char(b, '+'); tau_fmt_f64(b, z.im); }
    tau_buf_char(b, 'i');
}
static void tau_fmt_c64(tau_buf *b, tau_c64 z) {
    if (z.im < 0) { tau_fmt_f32(b, (float)z.re); tau_buf_char(b, '-'); tau_fmt_f32(b, (float)fabs(z.im)); }
    else { tau_fmt_f32(b, (float)z.re); tau_buf_char(b, '+'); tau_fmt_f32(b, (float)z.im); }
    tau_buf_char(b, 'i');
}

/* ---------------------------------------------------------------- tensors */

/* Dense, column-major (the first index varies fastest). Elements are stored as doubles (pairs
 * for complex); `single` tensors round every result to float, as the interpreter does. */
typedef struct { uint8_t rank, single, cplx; size_t dims[8]; double *data; } tau_tensor;

static uint64_t tau_rng = 0x9E3779B97F4A7C15ULL;
static double tau_next_random(void) {
    uint64_t x = tau_rng;
    x ^= x >> 12; x ^= x << 25; x ^= x >> 27;
    tau_rng = x;
    uint64_t r = x * 0x2545F4914F6CDD1DULL;
    return (double)(r >> 11) / 9007199254740992.0;
}

static size_t tau_t_len(const tau_tensor *t) {
    size_t n = 1;
    for (int i = 0; i < t->rank; i++) n *= t->dims[i];
    return n;
}
static tau_tensor tau_t_new(int rank, const size_t *dims, int cplx, int single) {
    tau_tensor t;
    memset(&t, 0, sizeof t);
    if (rank > 8) tau_panic_cstr("tensors have at most 8 dimensions");
    t.rank = (uint8_t)rank; t.cplx = (uint8_t)cplx; t.single = (uint8_t)single;
    for (int i = 0; i < rank; i++) t.dims[i] = dims[i];
    size_t n = tau_t_len(&t) * (cplx ? 2 : 1);
    t.data = (double *)calloc(n ? n : 1, sizeof(double));
    if (!t.data) tau_panic_cstr("out of memory");
    return t;
}
static void tau_t_free(tau_tensor *t) { free(t->data); memset(t, 0, sizeof *t); }
static tau_tensor tau_t_clone(const tau_tensor *t) {
    tau_tensor r = tau_t_new(t->rank, t->dims, t->cplx, t->single);
    memcpy(r.data, t->data, tau_t_len(t) * (t->cplx ? 2 : 1) * sizeof(double));
    return r;
}
/* kind: 0 zeros, 1 ones, 2 random */
static tau_tensor tau_t_fill(int kind, int rank, const size_t *dims, int cplx, int single) {
    tau_tensor t = tau_t_new(rank, dims, cplx, single);
    size_t n = tau_t_len(&t);
    for (size_t i = 0; i < n; i++) {
        double v = kind == 0 ? 0.0 : kind == 1 ? 1.0 : tau_next_random();
        if (kind == 2 && single) v = (double)(float)v;
        if (cplx) { t.data[2 * i] = v; t.data[2 * i + 1] = 0.0; } else t.data[i] = v;
    }
    return t;
}

static void tau_t_dims_str(tau_buf *b, const tau_tensor *t) {
    tau_buf_char(b, '[');
    for (int i = 0; i < t->rank; i++) { if (i) tau_buf_cstr(b, ", "); tau_fmt_u64(b, (uint64_t)t->dims[i]); }
    tau_buf_char(b, ']');
}
TAU_NORETURN TAU_COLD static void tau_t_shape_panic(const char *what, const tau_tensor *a, const tau_tensor *b) {
    tau_buf m = {0};
    tau_buf_cstr(&m, what);
    tau_t_dims_str(&m, a);
    tau_buf_cstr(&m, " and ");
    tau_t_dims_str(&m, b);
    tau_panic((const char *)m.ptr, m.len);
}

/* flat index of a multi-index; `read` picks the message the interpreter gives for a read */
static size_t tau_t_flat(const tau_tensor *t, const size_t *idx, int n, int read) {
    size_t stride = 1, flat = 0;
    for (int i = 0; i < n; i++) {
        if (idx[i] >= t->dims[i]) {
            if (!read) tau_panic_cstr("tensor index out of bounds");
            tau_buf m = {0};
            tau_buf_cstr(&m, "tensor index [");
            for (int j = 0; j < n; j++) { if (j) tau_buf_cstr(&m, ", "); tau_fmt_u64(&m, (uint64_t)idx[j]); }
            tau_buf_cstr(&m, "] is out of bounds");
            tau_panic((const char *)m.ptr, m.len);
        }
        flat += idx[i] * stride;
        stride *= t->dims[i];
    }
    return flat;
}

static double tau_t_round(int single, double x) { return single ? (double)(float)x : x; }

static tau_tensor tau_t_binop(char op, tau_tensor a, tau_tensor b) {
    if (a.rank != b.rank || memcmp(a.dims, b.dims, sizeof(size_t) * a.rank) != 0) tau_t_shape_panic("shape mismatch: ", &a, &b);
    tau_tensor r = tau_t_new(a.rank, a.dims, a.cplx, a.single);
    size_t n = tau_t_len(&a);
    if (!a.cplx) {
        for (size_t i = 0; i < n; i++) {
            double x = a.data[i], y = b.data[i], v;
            switch (op) { case '+': v = x + y; break; case '-': v = x - y; break; case '*': v = x * y; break; default: v = x / y; }
            r.data[i] = tau_t_round(a.single, v);
        }
    } else {
        if (op == '/') tau_panic_cstr("complex tensor division is not supported");
        for (size_t i = 0; i < n; i++) {
            double p = a.data[2 * i], q = a.data[2 * i + 1], r2 = b.data[2 * i], s = b.data[2 * i + 1];
            switch (op) {
                case '+': r.data[2 * i] = p + r2; r.data[2 * i + 1] = q + s; break;
                case '-': r.data[2 * i] = p - r2; r.data[2 * i + 1] = q - s; break;
                default: r.data[2 * i] = p * r2 - q * s; r.data[2 * i + 1] = p * s + q * r2;
            }
        }
    }
    return r;
}

static tau_tensor tau_t_scalar(char op, tau_tensor a, double k, int scalar_on_left) {
    if (a.cplx) tau_panic_cstr("scalar arithmetic on a complex tensor is not supported");
    tau_tensor r = tau_t_new(a.rank, a.dims, 0, a.single);
    size_t n = tau_t_len(&a);
    for (size_t i = 0; i < n; i++) {
        double x = scalar_on_left ? k : a.data[i], y = scalar_on_left ? a.data[i] : k, v;
        switch (op) { case '+': v = x + y; break; case '-': v = x - y; break; case '*': v = x * y; break; default: v = x / y; }
        r.data[i] = tau_t_round(a.single, v);
    }
    return r;
}

static tau_tensor tau_t_matmul(tau_tensor a, tau_tensor b) {
    if (a.cplx || b.cplx) tau_panic_cstr("`@` on complex tensors is not supported");
    size_t m = a.dims[0], k = a.dims[1];
    size_t k2 = b.dims[0], n = b.rank == 2 ? b.dims[1] : 1;
    if (k != k2) tau_t_shape_panic("shape mismatch for `@`: ", &a, &b);
    size_t odims[2] = { m, n };
    tau_tensor c = tau_t_new(b.rank == 2 ? 2 : 1, odims, 0, a.single);
    for (size_t j = 0; j < n; j++)
        for (size_t p = 0; p < k; p++) {
            double bkj = b.data[p + j * k];
            const double *col = &a.data[p * m];
            double *out = &c.data[j * m];
            for (size_t i = 0; i < m; i++) out[i] += col[i] * bkj;
        }
    if (a.single) { size_t tot = m * n; for (size_t i = 0; i < tot; i++) c.data[i] = (double)(float)c.data[i]; }
    return c;
}

/* Rust's f64 sum starts from -0.0 */
static double tau_t_dot(tau_tensor a, tau_tensor b) {
    if (a.dims[0] != b.dims[0]) tau_t_shape_panic("shape mismatch for `@`: ", &a, &b);
    double s = -0.0;
    for (size_t i = 0; i < a.dims[0]; i++) s += a.data[i] * b.data[i];
    return tau_t_round(a.single, s);
}
static double tau_t_trace(tau_tensor a) {
    size_t m = a.dims[0], k = m < a.dims[1] ? m : a.dims[1];
    double s = -0.0;
    for (size_t i = 0; i < k; i++) s += a.data[i + i * m];
    return s;
}
static tau_c128 tau_t_trace_c(tau_tensor a) {
    size_t m = a.dims[0], k = m < a.dims[1] ? m : a.dims[1];
    double re = -0.0, im = -0.0;
    for (size_t i = 0; i < k; i++) re += a.data[2 * (i + i * m)];
    for (size_t i = 0; i < k; i++) im += a.data[2 * (i + i * m) + 1];
    return (tau_c128){ re, im };
}

static void tau_t_fmt(tau_buf *b, tau_tensor t) {
#define TAU_CELL(i) do { \
        if (t.cplx) { double re = t.data[2 * (i)], im = t.data[2 * (i) + 1]; \
            tau_fmt_f64(b, re); if (im < 0) { tau_buf_char(b, '-'); tau_fmt_f64(b, -im); } else { tau_buf_char(b, '+'); tau_fmt_f64(b, im); } \
            tau_buf_char(b, 'i'); } \
        else if (t.single) tau_fmt_f32(b, (float)t.data[i]); else tau_fmt_f64(b, t.data[i]); } while (0)
    if (t.rank == 1) {
        tau_buf_char(b, '[');
        for (size_t i = 0; i < t.dims[0]; i++) { if (i) tau_buf_cstr(b, ", "); TAU_CELL(i); }
        tau_buf_char(b, ']');
    } else if (t.rank == 2) {
        size_t m = t.dims[0], n = t.dims[1];
        tau_buf_char(b, '[');
        for (size_t i = 0; i < m; i++) {
            if (i) tau_buf_cstr(b, ", ");
            tau_buf_char(b, '[');
            for (size_t j = 0; j < n; j++) { if (j) tau_buf_cstr(b, ", "); TAU_CELL(i + j * m); }
            tau_buf_char(b, ']');
        }
        tau_buf_char(b, ']');
    } else {
        tau_buf_cstr(b, "Tensor(shape=");
        tau_t_dims_str(b, &t);
        tau_buf_cstr(b, ", [");
        size_t n = tau_t_len(&t);
        for (size_t i = 0; i < n; i++) { if (i) tau_buf_cstr(b, ", "); TAU_CELL(i); }
        tau_buf_cstr(b, "])");
    }
#undef TAU_CELL
}

/* ---------------------------------------------------------------- linear algebra and FFT */

static tau_tensor tau_la_transpose(tau_tensor a) {
    size_t m = a.dims[0], n = a.dims[1];
    size_t od[2] = { n, m };
    tau_tensor r = tau_t_new(2, od, 0, a.single);
    for (size_t i = 0; i < m; i++) for (size_t j = 0; j < n; j++) r.data[j + i * n] = a.data[i + j * m];
    return r;
}

static double tau_la_det(tau_tensor a) {
    size_t n = a.dims[0];
    if (a.dims[0] != a.dims[1]) tau_panic_cstr("det needs a square matrix");
    double *d = (double *)malloc((n * n + 1) * sizeof(double));
    memcpy(d, a.data, n * n * sizeof(double));
    double det = 1.0;
    for (size_t c = 0; c < n; c++) {
        size_t piv = c;
        for (size_t r = c + 1; r < n; r++) if (fabs(d[r + c * n]) > fabs(d[piv + c * n])) piv = r;
        if (d[piv + c * n] == 0.0) { free(d); return 0.0; }
        if (piv != c) {
            for (size_t k = 0; k < n; k++) { double t = d[piv + k * n]; d[piv + k * n] = d[c + k * n]; d[c + k * n] = t; }
            det = -det;
        }
        det *= d[c + c * n];
        for (size_t r = c + 1; r < n; r++) {
            double factor = d[r + c * n] / d[c + c * n];
            for (size_t k = c; k < n; k++) d[r + k * n] -= factor * d[c + k * n];
        }
    }
    free(d);
    return det;
}

static tau_tensor tau_la_inv(tau_tensor a) {
    size_t n = a.dims[0];
    if (a.dims[0] != a.dims[1]) tau_panic_cstr("inv needs a square matrix");
    double *aug = (double *)calloc(n * 2 * n + 1, sizeof(double));
    for (size_t i = 0; i < n; i++) {
        for (size_t j = 0; j < n; j++) aug[i + j * n] = a.data[i + j * n];
        aug[i + (n + i) * n] = 1.0;
    }
    for (size_t c = 0; c < n; c++) {
        size_t piv = c;
        for (size_t r = c + 1; r < n; r++) if (fabs(aug[r + c * n]) > fabs(aug[piv + c * n])) piv = r;
        if (fabs(aug[piv + c * n]) < 1e-300) tau_panic_cstr("the matrix is singular and cannot be inverted");
        if (piv != c)
            for (size_t k = 0; k < 2 * n; k++) { double t = aug[piv + k * n]; aug[piv + k * n] = aug[c + k * n]; aug[c + k * n] = t; }
        double p = aug[c + c * n];
        for (size_t k = 0; k < 2 * n; k++) aug[c + k * n] /= p;
        for (size_t r = 0; r < n; r++)
            if (r != c) {
                double factor = aug[r + c * n];
                if (factor != 0.0) for (size_t k = 0; k < 2 * n; k++) aug[r + k * n] -= factor * aug[c + k * n];
            }
    }
    size_t od[2] = { n, n };
    tau_tensor out = tau_t_new(2, od, 0, a.single);
    for (size_t i = 0; i < n; i++) for (size_t j = 0; j < n; j++) out.data[i + j * n] = aug[i + (n + j) * n];
    free(aug);
    return out;
}

static void tau_la_eig(tau_tensor a, tau_tensor *vals, tau_tensor *vecs) {
    size_t n = a.dims[0];
    if (a.dims[0] != a.dims[1]) tau_panic_cstr("eig needs a square matrix");
    double *d = (double *)malloc((n * n + 1) * sizeof(double));
    memcpy(d, a.data, n * n * sizeof(double));
    for (size_t i = 0; i < n; i++)
        for (size_t j = 0; j < i; j++)
            if (fabs(d[i + j * n] - d[j + i * n]) > 1e-9 * (1.0 + fabs(d[i + j * n]))) tau_panic_cstr("eig supports symmetric matrices only");
    double *v = (double *)calloc(n * n + 1, sizeof(double));
    for (size_t i = 0; i < n; i++) v[i + i * n] = 1.0;
    for (int sweep = 0; sweep < 100; sweep++) {
        double off = 0.0;
        for (size_t i = 0; i < n; i++) for (size_t j = 0; j < n; j++) if (i != j) off += d[i + j * n] * d[i + j * n];
        if (off < 1e-22) break;
        for (size_t p = 0; p < n; p++)
            for (size_t q = p + 1; q < n; q++) {
                double apq = d[p + q * n];
                if (fabs(apq) < 1e-300) continue;
                double theta = (d[q + q * n] - d[p + p * n]) / (2.0 * apq);
                double sg = theta != theta ? theta : copysign(1.0, theta);
                double t = sg / (fabs(theta) + sqrt(theta * theta + 1.0));
                if (theta == 0.0) t = 1.0;
                double c = 1.0 / sqrt(t * t + 1.0);
                double s = t * c;
                for (size_t k = 0; k < n; k++) { double akp = d[k + p * n], akq = d[k + q * n]; d[k + p * n] = c * akp - s * akq; d[k + q * n] = s * akp + c * akq; }
                for (size_t k = 0; k < n; k++) { double apk = d[p + k * n], aqk = d[q + k * n]; d[p + k * n] = c * apk - s * aqk; d[q + k * n] = s * apk + c * aqk; }
                for (size_t k = 0; k < n; k++) { double vkp = v[k + p * n], vkq = v[k + q * n]; v[k + p * n] = c * vkp - s * vkq; v[k + q * n] = s * vkp + c * vkq; }
            }
    }
    /* ascending eigenvalues (a stable sort; unordered values count as equal) with their vectors */
    size_t *order = (size_t *)malloc((n + 1) * sizeof(size_t));
    for (size_t i = 0; i < n; i++) order[i] = i;
    for (size_t i = 1; i < n; i++) {
        size_t cur = order[i], j = i;
        while (j > 0 && d[cur + cur * n] < d[order[j - 1] + order[j - 1] * n]) { order[j] = order[j - 1]; j--; }
        order[j] = cur;
    }
    size_t vd[1] = { n }, md[2] = { n, n };
    *vals = tau_t_new(1, vd, 0, a.single);
    *vecs = tau_t_new(2, md, 0, a.single);
    for (size_t i = 0; i < n; i++) vals->data[i] = d[order[i] + order[i] * n];
    for (size_t nw = 0; nw < n; nw++) for (size_t k = 0; k < n; k++) vecs->data[k + nw * n] = v[k + order[nw] * n];
    free(d); free(v); free(order);
}

typedef struct { double re, im; } tau_cpx;

static tau_cpx *tau_fft_pow2(const tau_cpx *x, size_t n, double sign) {
    tau_cpx *out = (tau_cpx *)malloc((n ? n : 1) * sizeof(tau_cpx));
    if (n == 1) { out[0] = x[0]; return out; }
    size_t h = n / 2;
    tau_cpx *even = (tau_cpx *)malloc(h * sizeof(tau_cpx)), *odd = (tau_cpx *)malloc(h * sizeof(tau_cpx));
    for (size_t i = 0; i < h; i++) { even[i] = x[2 * i]; odd[i] = x[2 * i + 1]; }
    tau_cpx *e = tau_fft_pow2(even, h, sign), *o = tau_fft_pow2(odd, h, sign);
    for (size_t k = 0; k < h; k++) {
        double ang = sign * 2.0 * M_PI * (double)k / (double)n;
        double sn = sin(ang), cs = cos(ang);
        tau_cpx t = { o[k].re * cs - o[k].im * sn, o[k].re * sn + o[k].im * cs };
        out[k] = (tau_cpx){ e[k].re + t.re, e[k].im + t.im };
        out[k + h] = (tau_cpx){ e[k].re - t.re, e[k].im - t.im };
    }
    free(even); free(odd); free(e); free(o);
    return out;
}

static tau_cpx *tau_dft(const tau_cpx *x, size_t n, int inverse) {
    tau_cpx *out;
    double sign = inverse ? 1.0 : -1.0;
    if (n <= 1) {
        out = (tau_cpx *)malloc((n ? n : 1) * sizeof(tau_cpx));
        if (n) out[0] = x[0];
        return out;
    }
    if ((n & (n - 1)) == 0) out = tau_fft_pow2(x, n, sign);
    else {
        out = (tau_cpx *)malloc(n * sizeof(tau_cpx));
        for (size_t k = 0; k < n; k++) {
            double re = 0.0, im = 0.0;
            for (size_t j = 0; j < n; j++) {
                double ang = sign * 2.0 * M_PI * (double)((k * j) % n) / (double)n;
                double sn = sin(ang), cs = cos(ang);
                re += x[j].re * cs - x[j].im * sn;
                im += x[j].re * sn + x[j].im * cs;
            }
            out[k] = (tau_cpx){ re, im };
        }
    }
    if (inverse) for (size_t i = 0; i < n; i++) { out[i].re /= (double)n; out[i].im /= (double)n; }
    return out;
}

static tau_tensor tau_fft(tau_tensor a) {
    size_t n = a.dims[0];
    tau_cpx *in = (tau_cpx *)malloc((n ? n : 1) * sizeof(tau_cpx));
    for (size_t i = 0; i < n; i++) in[i] = (tau_cpx){ a.data[i], 0.0 };
    tau_cpx *out = tau_dft(in, n, 0);
    size_t od[1] = { n };
    tau_tensor r = tau_t_new(1, od, 1, 0);
    for (size_t i = 0; i < n; i++) { r.data[2 * i] = out[i].re; r.data[2 * i + 1] = out[i].im; }
    free(in); free(out);
    return r;
}

static tau_tensor tau_ifft(tau_tensor a) {
    size_t n = a.dims[0];
    tau_cpx *in = (tau_cpx *)malloc((n ? n : 1) * sizeof(tau_cpx));
    for (size_t i = 0; i < n; i++) in[i] = (tau_cpx){ a.data[2 * i], a.data[2 * i + 1] };
    tau_cpx *out = tau_dft(in, n, 1);
    size_t od[1] = { n };
    tau_tensor r = tau_t_new(1, od, 0, 0);
    for (size_t i = 0; i < n; i++) r.data[i] = out[i].re;
    free(in); free(out);
    return r;
}

/* ---------------------------------------------------------------- files and standard input */

#include <errno.h>

/* The text of an I/O error, as the interpreter prints it (Rust's io::Error): `strerror (os error N)`.
 * Code -1 is invalid UTF-8. */
static tau_buf tau_io_msg(int code) {
    tau_buf b = {0};
    if (code == -1) { tau_buf_cstr(&b, "stream did not contain valid UTF-8"); return b; }
    tau_buf_cstr(&b, strerror(code));
    tau_buf_cstr(&b, " (os error ");
    tau_fmt_i32(&b, (int32_t)code);
    tau_buf_char(&b, ')');
    return b;
}

static int tau_utf8_valid(const uint8_t *p, size_t n) {
    size_t i = 0;
    while (i < n) {
        uint8_t c = p[i];
        size_t k;
        uint32_t cp;
        if (c < 0x80) { i++; continue; }
        else if (c >= 0xC2 && c <= 0xDF) { k = 1; cp = c & 0x1F; }
        else if (c >= 0xE0 && c <= 0xEF) { k = 2; cp = c & 0x0F; }
        else if (c >= 0xF0 && c <= 0xF4) { k = 3; cp = c & 0x07; }
        else return 0;
        if (i + k >= n) return 0;
        for (size_t j = 1; j <= k; j++) {
            if ((p[i + j] & 0xC0) != 0x80) return 0;
            cp = (cp << 6) | (p[i + j] & 0x3F);
        }
        if ((k == 2 && cp < 0x800) || (k == 3 && (cp < 0x10000 || cp > 0x10FFFF)) || (cp >= 0xD800 && cp <= 0xDFFF)) return 0;
        i += k + 1;
    }
    return 1;
}

static char *tau_cpath(tau_str p) {
    char *c = (char *)malloc(p.len + 1);
    if (!c) tau_panic_cstr("out of memory");
    if (p.len) memcpy(c, p.ptr, p.len);
    c[p.len] = 0;
    return c;
}

/* 0 on success, else an errno value (or -1 for invalid UTF-8) */
static int tau_fs_read(tau_str path, tau_buf *out) {
    char *c = tau_cpath(path);
    FILE *f = fopen(c, "rb");
    free(c);
    if (!f) return errno ? errno : EIO;
    uint8_t chunk[4096];
    size_t n;
    while ((n = fread(chunk, 1, sizeof chunk, f)) > 0) tau_buf_push(out, chunk, n);
    int err = ferror(f) ? (errno ? errno : EIO) : 0;
    fclose(f);
    if (err) return err;
    if (!tau_utf8_valid(out->ptr, out->len)) { free(out->ptr); out->ptr = 0; out->len = out->cap = 0; return -1; }
    return 0;
}

static int tau_fs_write(tau_str path, tau_str data) {
    char *c = tau_cpath(path);
    FILE *f = fopen(c, "wb");
    free(c);
    if (!f) return errno ? errno : EIO;
    int err = 0;
    if (data.len && fwrite(data.ptr, 1, data.len, f) != data.len) err = errno ? errno : EIO;
    if (fclose(f) != 0 && !err) err = errno ? errno : EIO;
    return err;
}

static bool tau_fs_exists(tau_str path) {
    char *c = tau_cpath(path);
    bool ok = access(c, F_OK) == 0;
    free(c);
    return ok;
}

/* one line of standard input without its line ending (empty at end of input) */
static int tau_read_line(tau_buf *out) {
    int ch;
    while ((ch = getchar()) != EOF) {
        if (ch == '\n') break;
        uint8_t b = (uint8_t)ch;
        tau_buf_push(out, &b, 1);
    }
    while (out->len > 0 && out->ptr[out->len - 1] == '\r') out->len--;
    if (ch == EOF && ferror(stdin)) return errno ? errno : EIO;
    if (!tau_utf8_valid(out->ptr, out->len)) { free(out->ptr); out->ptr = 0; out->len = out->cap = 0; return -1; }
    return 0;
}

/* ---------------------------------------------------------------- program entry, stack overflow */

static uintptr_t tau_stack_lo;
static int tau_run_main(void);
static int tau_exit_status;

static void tau_segv(int sig, siginfo_t *si, void *ctx) {
    (void)ctx;
    uintptr_t a = (uintptr_t)si->si_addr;
    if (a + 65536 >= tau_stack_lo && a < tau_stack_lo + 65536) {
        static const char m[] = "panic: stack overflow: the program recursed too deeply\n";
        tau_flush_stdout();
        (void)!write(2, m, sizeof m - 1);
        _exit(101);
    }
    signal(sig, SIG_DFL);
}

static void *tau_thread_main(void *arg) {
    (void)arg;
    pthread_attr_t at;
    void *lo = 0;
    size_t sz = 0;
    if (pthread_getattr_np(pthread_self(), &at) == 0) {
        pthread_attr_getstack(&at, &lo, &sz);
        pthread_attr_destroy(&at);
        tau_stack_lo = (uintptr_t)lo;
    }
    stack_t ss;
    static char tau_altstack[1 << 16];
    ss.ss_sp = tau_altstack;
    ss.ss_size = 1 << 16;
    ss.ss_flags = 0;
    sigaltstack(&ss, 0);
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = tau_segv;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGSEGV, &sa, 0);
    sigaction(SIGBUS, &sa, 0);
    tau_exit_status = tau_run_main();
    return 0;
}

int main(void) {
    pthread_attr_t at;
    pthread_t th;
    pthread_attr_init(&at);
    size_t sizes[] = {(size_t)1 << 30, (size_t)1 << 28, (size_t)1 << 26};
    int started = 0;
    for (int i = 0; i < 3 && !started; i++) {
        pthread_attr_setstacksize(&at, sizes[i]);
        if (pthread_create(&th, &at, tau_thread_main, 0) == 0) started = 1;
    }
    if (!started) { tau_thread_main(0); }
    else pthread_join(th, 0);
    tau_flush_stdout();
    return tau_exit_status;
}
/* ===== end of runtime ===== */
