"""Independent reference implementations of the programs in this directory.

Python floats are IEEE-754 doubles, like Tau's f64, and operations are done in the same
order, so the printed results must match Tau's digit for digit. `tests/programs.rs`
compares Tau's output with the expected output recorded here.
"""
import math


def primes():
    n = 100
    sieve = [True] * 101
    sieve[0] = sieve[1] = False
    i = 2
    while i * i <= n:
        if sieve[i]:
            j = i * i
            while j <= n:
                sieve[j] = False
                j += i
        i += 1
    ps = [k for k in range(101) if sieve[k]]
    return f"[{', '.join(map(str, ps))}]\n{len(ps)} primes up to {n}\n"


def collatz():
    def steps(n):
        c = 0
        while n != 1:
            n = n // 2 if n % 2 == 0 else 3 * n + 1
            c += 1
        return c

    best, best_steps = 1, 0
    for n in range(1, 1000):
        s = steps(n)
        if s > best_steps:
            best, best_steps = n, s
    return f"longest chain under 1000: {best} with {best_steps} steps\n"


def sort():
    state = 42
    data = []
    for _ in range(20):
        state = (state * 1103515245 + 12345) % 2147483648
        data.append(state % 100)
    show = lambda v: f"[{', '.join(map(str, v))}]\n"
    return show(data) + show(sorted(data))


def fmt(x):
    """Tau prints a float as the shortest digits that read back exactly, never in
    scientific notation, with ".0" added when the value is a whole number."""
    from decimal import Decimal
    text = format(Decimal(repr(x)), "f")
    return text if "." in text else text + ".0"


BLOCK = 4096


def blocked(values, combine):
    """A parallel reduction: blocks of 4096 items, each reduced in order from its first
    item, block results combined in block order (so the answer never depends on the
    number of threads)."""
    blocks, cur, n = [], None, 0
    for x in values:
        cur = x if cur is None else combine(cur, x)
        n += 1
        if n == BLOCK:
            blocks.append(cur)
            cur, n = None, 0
    if cur is not None:
        blocks.append(cur)
    acc = None
    for b in blocks:
        acc = b if acc is None else combine(acc, b)
    return acc


def integrate():
    n = 1000000
    a, b = 0.0, math.pi
    h = (b - a) / float(n)
    s = blocked((math.sin(a + float(i) * h) for i in range(1, n)), lambda p, q: p + q)
    return fmt(h * (s + 0.5 * (math.sin(a) + math.sin(b)))) + "\n"


def nbody():
    G = 6.674e-11

    class V:
        def __init__(s, x, y, z):
            s.x, s.y, s.z = x, y, z

        def __add__(s, o): return V(s.x + o.x, s.y + o.y, s.z + o.z)
        def __sub__(s, o): return V(s.x - o.x, s.y - o.y, s.z - o.z)
        def scale(s, k): return V(s.x * k, s.y * k, s.z * k)
        def dot(s, o): return s.x * o.x + s.y * o.y + s.z * o.z
        def norm(s): return math.sqrt(s.dot(s))

    class B:
        def __init__(s, pos, vel, mass): s.pos, s.vel, s.mass = pos, vel, mass

    bodies = [B(V(0.0, 0.0, 0.0), V(0.0, 0.0, 0.0), 1.0e10), B(V(1.0, 0.0, 0.0), V(0.0, 1.0, 0.0), 1.0)]
    dt = 0.01
    for _ in range(1000):
        for i in range(len(bodies)):
            acc = V(0.0, 0.0, 0.0)
            for j in range(len(bodies)):
                if j != i:
                    d = bodies[j].pos - bodies[i].pos
                    acc = acc + d.scale(G * bodies[j].mass / math.pow(d.norm(), 3.0))
            bodies[i].vel = bodies[i].vel + acc.scale(dt)
        for b in bodies:
            b.pos = b.pos + b.vel.scale(dt)
    e = 0.0
    for b in bodies:
        e += 0.5 * b.mass * b.vel.dot(b.vel)
    return f"energy = {fmt(e)}\n"


def bank():
    out = []
    balance, frozen = 100, False

    def withdraw(amount):
        nonlocal balance
        if frozen:
            return "account frozen"
        if balance < amount:
            return f"short by {amount - balance}"
        balance -= amount
        return f"ok, {balance} left"

    for amount in (30, 50, 40):
        out.append(withdraw(amount))
    frozen = True
    out.append(withdraw(1))
    out.append(f"final balance: {balance}")
    return "\n".join(out) + "\n"


def wordcount():
    text = open("examples/run/words.txt").read()
    counts = {}
    for w in text.split():
        counts[w.lower()] = counts.get(w.lower(), 0) + 1
    top = sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))[:4]
    return "".join(f"{w}: {n}\n" for w, n in top)


PROGRAMS = dict(primes=primes, collatz=collatz, sort=sort, integrate=integrate, nbody=nbody, bank=bank, wordcount=wordcount)

if __name__ == "__main__":
    import sys
    for name in sys.argv[1:] or PROGRAMS:
        print(f"=== {name}")
        print(PROGRAMS[name](), end="")
