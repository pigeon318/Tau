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

/* ---------------------------------------------------------------- program entry, stack overflow */

static uintptr_t tau_stack_lo;
static void tau_run_main(void);

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
    ss.ss_sp = malloc(1 << 16);
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
    tau_run_main();
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
    return 0;
}
/* ===== end of runtime ===== */
