//! Tensors: construction, element-wise arithmetic, matrix products, and the
//! linear-algebra / FFT routines of `math.linalg` and `math.fft`.
//!
//! Storage is column-major (the first index varies fastest), as in BLAS and Fortran.

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::ast::BinOp;

fn round(single: bool, x: f64) -> f64 {
    if single {
        x as f32 as f64
    } else {
        x
    }
}

impl<'a> Interp<'a> {
    pub fn tensor_ctor(&mut self, kind: &str, e: &'a Expr, args: &'a [Expr], f: &Frame<'a>) -> R<'a, Value<'a>> {
        let Ty::Tensor(elem, dims) = self.ty_of(e, f) else {
            return panic("a tensor constructor needs a tensor type");
        };
        let sizes: Vec<usize> = if args.is_empty() {
            let mut out = Vec::new();
            for d in &dims {
                match d {
                    Dim::Fixed(n) => out.push(*n as usize),
                    _ => return panic("this tensor shape is not fixed; pass the sizes"),
                }
            }
            out
        } else {
            let mut out = Vec::new();
            for a in args {
                match self.eval(a, f)? {
                    Value::Int(n) if n >= 0 => out.push(n as usize),
                    _ => return panic("tensor sizes must be non-negative integers"),
                }
            }
            out
        };
        let n: usize = sizes.iter().product();
        let single = matches!(*elem, Ty::Float(FloatTy::F32) | Ty::Complex(FloatTy::F32));
        let complex = matches!(*elem, Ty::Complex(_));
        let next = |this: &mut Self| match kind {
            "tensor.zeros" => 0.0,
            "tensor.ones" => 1.0,
            _ => round(single, this.next_random()),
        };
        let data = if complex {
            TData::C((0..n).map(|_| (next(self), 0.0)).collect())
        } else {
            TData::F((0..n).map(|_| next(self)).collect())
        };
        Ok(Value::Tensor(Rc::new(RefCell::new(Tensor { dims: sizes, data, single }))))
    }

    pub fn tensor_text(&self, t: &Tensor) -> String {
        let cell = |i: usize| -> String {
            match &t.data {
                TData::F(d) => self.float_text(d[i]),
                TData::C(d) => {
                    let (a, b) = d[i];
                    if b < 0.0 {
                        format!("{}-{}i", self.float_text(a), self.float_text(-b))
                    } else {
                        format!("{}+{}i", self.float_text(a), self.float_text(b))
                    }
                }
            }
        };
        match t.dims.len() {
            1 => format!("[{}]", (0..t.dims[0]).map(cell).collect::<Vec<_>>().join(", ")),
            2 => {
                let (m, n) = (t.dims[0], t.dims[1]);
                let rows: Vec<String> = (0..m)
                    .map(|i| format!("[{}]", (0..n).map(|j| cell(i + j * m)).collect::<Vec<_>>().join(", ")))
                    .collect();
                format!("[{}]", rows.join(", "))
            }
            _ => format!("Tensor(shape={:?}, [{}])", t.dims, (0..t.len()).map(cell).collect::<Vec<_>>().join(", ")),
        }
    }

    pub fn tensor_trace(&self, v: &Value<'a>) -> R<'a, Value<'a>> {
        let Value::Tensor(t) = v else { return panic("trace needs a tensor") };
        let t = t.borrow();
        if t.dims.len() != 2 {
            return panic("trace needs a matrix");
        }
        let (m, n) = (t.dims[0], t.dims[1]);
        let k = m.min(n);
        Ok(match &t.data {
            TData::F(d) => Value::Float((0..k).map(|i| d[i + i * m]).sum()),
            TData::C(d) => Value::Complex((0..k).map(|i| d[i + i * m].0).sum(), (0..k).map(|i| d[i + i * m].1).sum()),
        })
    }

    pub fn tensor_binop(&mut self, op: BinOp, l: Value<'a>, r: Value<'a>) -> R<'a, Value<'a>> {
        if op == BinOp::MatMul {
            return self.matmul(&l, &r);
        }
        let apply = |x: f64, y: f64| -> R<'a, f64> {
            Ok(match op {
                BinOp::Add => x + y,
                BinOp::Sub => x - y,
                BinOp::Mul => x * y,
                BinOp::Div => x / y,
                _ => return panic(format!("`{}` is not defined on tensors", op.symbol())),
            })
        };
        match (&l, &r) {
            (Value::Tensor(a), Value::Tensor(b)) => {
                let (a, b) = (a.borrow(), b.borrow());
                if a.dims != b.dims {
                    return panic(format!("shape mismatch: {:?} and {:?}", a.dims, b.dims));
                }
                let single = a.single;
                let data = match (&a.data, &b.data) {
                    (TData::F(x), TData::F(y)) => {
                        let mut out = Vec::with_capacity(x.len());
                        for (p, q) in x.iter().zip(y) {
                            out.push(round(single, apply(*p, *q)?));
                        }
                        TData::F(out)
                    }
                    (TData::C(x), TData::C(y)) => {
                        let mut out = Vec::with_capacity(x.len());
                        for ((p, q), (r2, s)) in x.iter().zip(y) {
                            out.push(match op {
                                BinOp::Add => (p + r2, q + s),
                                BinOp::Sub => (p - r2, q - s),
                                BinOp::Mul => (p * r2 - q * s, p * s + q * r2),
                                _ => return panic("complex tensor division is not supported"),
                            });
                        }
                        TData::C(out)
                    }
                    _ => return panic("cannot combine real and complex tensors"),
                };
                Ok(Value::Tensor(Rc::new(RefCell::new(Tensor { dims: a.dims.clone(), data, single }))))
            }
            (Value::Tensor(a), s) | (s, Value::Tensor(a)) => {
                let scalar_on_left = !matches!(l, Value::Tensor(_));
                let a = a.borrow();
                let k = match s {
                    Value::Float(x) => *x,
                    Value::Int(i) => *i as f64,
                    _ => return panic("tensor arithmetic needs a tensor or a real number"),
                };
                let TData::F(x) = &a.data else { return panic("scalar arithmetic on a complex tensor is not supported") };
                let mut out = Vec::with_capacity(x.len());
                for p in x {
                    let v = if scalar_on_left { apply(k, *p)? } else { apply(*p, k)? };
                    out.push(round(a.single, v));
                }
                Ok(Value::Tensor(Rc::new(RefCell::new(Tensor { dims: a.dims.clone(), data: TData::F(out), single: a.single }))))
            }
            _ => panic("a tensor operation needs a tensor"),
        }
    }

    fn matmul(&mut self, l: &Value<'a>, r: &Value<'a>) -> R<'a, Value<'a>> {
        let (Value::Tensor(a), Value::Tensor(b)) = (l, r) else {
            return panic("`@` needs two tensors");
        };
        let (a, b) = (a.borrow(), b.borrow());
        let (TData::F(x), TData::F(y)) = (&a.data, &b.data) else {
            return panic("`@` on complex tensors is not supported");
        };
        let single = a.single;
        match (a.dims.len(), b.dims.len()) {
            (2, 2) | (2, 1) => {
                let (m, k) = (a.dims[0], a.dims[1]);
                let (k2, n) = if b.dims.len() == 2 { (b.dims[0], b.dims[1]) } else { (b.dims[0], 1) };
                if k != k2 {
                    return panic(format!("shape mismatch for `@`: {:?} and {:?}", a.dims, b.dims));
                }
                let mut c = vec![0.0f64; m * n];
                for j in 0..n {
                    for p in 0..k {
                        let bkj = y[p + j * k];
                        let col = &x[p * m..(p + 1) * m];
                        let out = &mut c[j * m..(j + 1) * m];
                        for i in 0..m {
                            out[i] += col[i] * bkj;
                        }
                    }
                }
                if single {
                    c.iter_mut().for_each(|v| *v = *v as f32 as f64);
                }
                let dims = if b.dims.len() == 2 { vec![m, n] } else { vec![m] };
                Ok(Value::Tensor(Rc::new(RefCell::new(Tensor { dims, data: TData::F(c), single }))))
            }
            (1, 1) => {
                if a.dims[0] != b.dims[0] {
                    return panic(format!("shape mismatch for `@`: {:?} and {:?}", a.dims, b.dims));
                }
                Ok(Value::Float(round(single, x.iter().zip(y).map(|(p, q)| p * q).sum())))
            }
            (p, q) => panic(format!("`@` is defined for vectors and matrices, not ranks {p} and {q}")),
        }
    }

    // ------------------------------------------------------------ linear algebra

    fn matrix_of(&self, v: &Value<'a>) -> R<'a, (usize, usize, Vec<f64>, bool)> {
        let Value::Tensor(t) = v else { return panic("expected a matrix") };
        let t = t.borrow();
        let TData::F(d) = &t.data else { return panic("expected a real matrix") };
        if t.dims.len() != 2 {
            return panic("expected a matrix (rank 2)");
        }
        Ok((t.dims[0], t.dims[1], d.clone(), t.single))
    }

    fn make(&self, dims: Vec<usize>, d: Vec<f64>, single: bool) -> Value<'a> {
        Value::Tensor(Rc::new(RefCell::new(Tensor { dims, data: TData::F(d), single })))
    }

    pub fn linalg(&mut self, key: &str, a: &[Value<'a>]) -> R<'a, Value<'a>> {
        match key {
            "math.linalg::transpose" => {
                let (m, n, d, single) = self.matrix_of(&a[0])?;
                let mut out = vec![0.0; m * n];
                for i in 0..m {
                    for j in 0..n {
                        out[j + i * n] = d[i + j * m];
                    }
                }
                Ok(self.make(vec![n, m], out, single))
            }
            "math.linalg::det" => {
                let (m, n, mut d, _) = self.matrix_of(&a[0])?;
                if m != n {
                    return panic("det needs a square matrix");
                }
                let mut det = 1.0;
                for c in 0..n {
                    let mut piv = c;
                    for r in c + 1..n {
                        if d[r + c * n].abs() > d[piv + c * n].abs() {
                            piv = r;
                        }
                    }
                    if d[piv + c * n] == 0.0 {
                        return Ok(Value::Float(0.0));
                    }
                    if piv != c {
                        for k in 0..n {
                            d.swap(piv + k * n, c + k * n);
                        }
                        det = -det;
                    }
                    det *= d[c + c * n];
                    for r in c + 1..n {
                        let factor = d[r + c * n] / d[c + c * n];
                        for k in c..n {
                            d[r + k * n] -= factor * d[c + k * n];
                        }
                    }
                }
                Ok(Value::Float(det))
            }
            "math.linalg::inv" => {
                let (m, n, d, single) = self.matrix_of(&a[0])?;
                if m != n {
                    return panic("inv needs a square matrix");
                }
                // Gauss-Jordan on [A | I]
                let mut aug = vec![0.0; n * 2 * n];
                for i in 0..n {
                    for j in 0..n {
                        aug[i + j * n] = d[i + j * n];
                    }
                    aug[i + (n + i) * n] = 1.0;
                }
                for c in 0..n {
                    let mut piv = c;
                    for r in c + 1..n {
                        if aug[r + c * n].abs() > aug[piv + c * n].abs() {
                            piv = r;
                        }
                    }
                    if aug[piv + c * n].abs() < 1e-300 {
                        return panic("the matrix is singular and cannot be inverted");
                    }
                    if piv != c {
                        for k in 0..2 * n {
                            aug.swap(piv + k * n, c + k * n);
                        }
                    }
                    let p = aug[c + c * n];
                    for k in 0..2 * n {
                        aug[c + k * n] /= p;
                    }
                    for r in 0..n {
                        if r != c {
                            let factor = aug[r + c * n];
                            if factor != 0.0 {
                                for k in 0..2 * n {
                                    aug[r + k * n] -= factor * aug[c + k * n];
                                }
                            }
                        }
                    }
                }
                let mut out = vec![0.0; n * n];
                for i in 0..n {
                    for j in 0..n {
                        out[i + j * n] = aug[i + (n + j) * n];
                    }
                }
                Ok(self.make(vec![n, n], out, single))
            }
            "math.linalg::eig" => {
                let (m, n, mut d, single) = self.matrix_of(&a[0])?;
                if m != n {
                    return panic("eig needs a square matrix");
                }
                for i in 0..n {
                    for j in 0..i {
                        if (d[i + j * n] - d[j + i * n]).abs() > 1e-9 * (1.0 + d[i + j * n].abs()) {
                            return panic("eig supports symmetric matrices only");
                        }
                    }
                }
                // Jacobi rotations: A = V diag(w) V^T
                let mut v = vec![0.0; n * n];
                for i in 0..n {
                    v[i + i * n] = 1.0;
                }
                for _sweep in 0..100 {
                    let mut off = 0.0;
                    for i in 0..n {
                        for j in 0..n {
                            if i != j {
                                off += d[i + j * n] * d[i + j * n];
                            }
                        }
                    }
                    if off < 1e-22 {
                        break;
                    }
                    for p in 0..n {
                        for q in p + 1..n {
                            let apq = d[p + q * n];
                            if apq.abs() < 1e-300 {
                                continue;
                            }
                            let theta = (d[q + q * n] - d[p + p * n]) / (2.0 * apq);
                            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                            let t = if theta == 0.0 { 1.0 } else { t };
                            let c = 1.0 / (t * t + 1.0).sqrt();
                            let s = t * c;
                            for k in 0..n {
                                let (akp, akq) = (d[k + p * n], d[k + q * n]);
                                d[k + p * n] = c * akp - s * akq;
                                d[k + q * n] = s * akp + c * akq;
                            }
                            for k in 0..n {
                                let (apk, aqk) = (d[p + k * n], d[q + k * n]);
                                d[p + k * n] = c * apk - s * aqk;
                                d[q + k * n] = s * apk + c * aqk;
                            }
                            for k in 0..n {
                                let (vkp, vkq) = (v[k + p * n], v[k + q * n]);
                                v[k + p * n] = c * vkp - s * vkq;
                                v[k + q * n] = s * vkp + c * vkq;
                            }
                        }
                    }
                }
                // ascending eigenvalues, with matching eigenvector columns
                let mut order: Vec<usize> = (0..n).collect();
                order.sort_by(|&i, &j| d[i + i * n].partial_cmp(&d[j + j * n]).unwrap_or(std::cmp::Ordering::Equal));
                let vals: Vec<f64> = order.iter().map(|&i| d[i + i * n]).collect();
                let mut vecs = vec![0.0; n * n];
                for (new, &old) in order.iter().enumerate() {
                    for k in 0..n {
                        vecs[k + new * n] = v[k + old * n];
                    }
                }
                Ok(Value::tuple(vec![self.make(vec![n], vals, single), self.make(vec![n, n], vecs, single)]))
            }
            "math.fft::fft" => {
                let Value::Tensor(t) = &a[0] else { return panic("fft needs a tensor") };
                let t = t.borrow();
                let TData::F(d) = &t.data else { return panic("fft needs a real signal") };
                let input: Vec<(f64, f64)> = d.iter().map(|x| (*x, 0.0)).collect();
                let out = dft(&input, false);
                Ok(Value::Tensor(Rc::new(RefCell::new(Tensor { dims: vec![out.len()], data: TData::C(out), single: false }))))
            }
            "math.fft::ifft" => {
                let Value::Tensor(t) = &a[0] else { return panic("ifft needs a tensor") };
                let t = t.borrow();
                let TData::C(d) = &t.data else { return panic("ifft needs a complex spectrum") };
                let out = dft(d, true);
                let real: Vec<f64> = out.iter().map(|(re, _)| *re).collect();
                Ok(self.make(vec![real.len()], real, false))
            }
            other => panic(format!("`{other}` is not implemented")),
        }
    }
}

/// Discrete Fourier transform: radix-2 when the length is a power of two, otherwise
/// the direct O(n^2) sum. The inverse is scaled by 1/n.
fn dft(x: &[(f64, f64)], inverse: bool) -> Vec<(f64, f64)> {
    let n = x.len();
    if n <= 1 {
        return x.to_vec();
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut out = if n.is_power_of_two() { fft_pow2(x, sign) } else { dft_direct(x, sign) };
    if inverse {
        for v in out.iter_mut() {
            v.0 /= n as f64;
            v.1 /= n as f64;
        }
    }
    out
}

fn dft_direct(x: &[(f64, f64)], sign: f64) -> Vec<(f64, f64)> {
    let n = x.len();
    (0..n)
        .map(|k| {
            let mut re = 0.0;
            let mut im = 0.0;
            for (j, (a, b)) in x.iter().enumerate() {
                let ang = sign * 2.0 * std::f64::consts::PI * ((k * j) % n) as f64 / n as f64;
                let (s, c) = ang.sin_cos();
                re += a * c - b * s;
                im += a * s + b * c;
            }
            (re, im)
        })
        .collect()
}

fn fft_pow2(x: &[(f64, f64)], sign: f64) -> Vec<(f64, f64)> {
    let n = x.len();
    if n == 1 {
        return x.to_vec();
    }
    let even: Vec<(f64, f64)> = x.iter().step_by(2).copied().collect();
    let odd: Vec<(f64, f64)> = x.iter().skip(1).step_by(2).copied().collect();
    let (e, o) = (fft_pow2(&even, sign), fft_pow2(&odd, sign));
    let mut out = vec![(0.0, 0.0); n];
    for k in 0..n / 2 {
        let ang = sign * 2.0 * std::f64::consts::PI * k as f64 / n as f64;
        let (s, c) = ang.sin_cos();
        let t = (o[k].0 * c - o[k].1 * s, o[k].0 * s + o[k].1 * c);
        out[k] = (e[k].0 + t.0, e[k].1 + t.1);
        out[k + n / 2] = (e[k].0 - t.0, e[k].1 - t.1);
    }
    out
}
