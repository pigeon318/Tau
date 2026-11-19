//! Tensors of integers and of `bool`, and what is common to every kind of tensor element:
//! comparisons (`a > 0` gives a tensor of `bool`), choosing elements (`select`), conversions.
//!
//! An integer tensor keeps its integer type, and every result is checked against its range, exactly
//! like the same operation on single integers (the element operations are `int_op`).

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::ast::BinOp;

impl<'a> Interp<'a> {
    /// A new tensor value.
    pub(super) fn new_tensor(dims: Vec<usize>, data: TData, single: bool) -> Value<'a> {
        Value::Tensor(Rc::new(RefCell::new(Tensor { dims, data, single })))
    }

    /// Element `i` of a tensor, as a value.
    pub(super) fn tensor_elem(data: &TData, i: usize) -> Value<'a> {
        match data {
            TData::F(d) => Value::Float(d[i]),
            TData::C(d) => Value::Complex(d[i].0, d[i].1),
            TData::I(_, d) => Value::Int(d[i]),
            TData::B(d) => Value::Bool(d[i]),
        }
    }

    pub(super) fn data_len(data: &TData) -> usize {
        match data {
            TData::F(d) => d.len(),
            TData::C(d) => d.len(),
            TData::I(_, d) => d.len(),
            TData::B(d) => d.len(),
        }
    }

    /// The elements at the given positions, in that order.
    pub(super) fn gather(data: &TData, pos: &[usize]) -> TData {
        match data {
            TData::F(d) => TData::F(pos.iter().map(|&p| d[p]).collect()),
            TData::C(d) => TData::C(pos.iter().map(|&p| d[p]).collect()),
            TData::I(it, d) => TData::I(*it, pos.iter().map(|&p| d[p]).collect()),
            TData::B(d) => TData::B(pos.iter().map(|&p| d[p]).collect()),
        }
    }

    /// The tensor's element type, from how the checker typed an expression.
    pub(super) fn elem_kind(elem: &Ty) -> Option<ElemKind> {
        match elem {
            Ty::Int(it) => Some(ElemKind::Int(*it)),
            Ty::Bool => Some(ElemKind::Bool),
            _ => None,
        }
    }

    // ------------------------------------------------------------ constructors

    /// `zeros`, `ones` for integers and `bool`.
    pub(super) fn int_fill(kind: &str, n: usize, ek: ElemKind) -> TData {
        let one = kind == "tensor.ones";
        match ek {
            ElemKind::Int(it) => TData::I(it, vec![one as i128; n]),
            ElemKind::Bool => TData::B(vec![one; n]),
        }
    }

    fn elem_of_value(&self, v: &Value<'a>, ek: ElemKind) -> R<'a, Value<'a>> {
        let v = self.deref(v.clone())?;
        match (&v, ek) {
            (Value::Int(_), ElemKind::Int(_)) | (Value::Bool(_), ElemKind::Bool) => Ok(v),
            (other, _) => panic(format!("expected {}, found a {}", if matches!(ek, ElemKind::Bool) { "a bool" } else { "an integer" }, other.type_name())),
        }
    }

    fn data_from_values(&self, vals: Vec<Value<'a>>, ek: ElemKind) -> R<'a, TData> {
        Ok(match ek {
            ElemKind::Int(it) => {
                let mut out = Vec::with_capacity(vals.len());
                for v in &vals {
                    match self.elem_of_value(v, ek)? {
                        Value::Int(i) => out.push(i),
                        _ => unreachable!(),
                    }
                }
                TData::I(it, out)
            }
            ElemKind::Bool => {
                let mut out = Vec::with_capacity(vals.len());
                for v in &vals {
                    match self.elem_of_value(v, ek)? {
                        Value::Bool(b) => out.push(b),
                        _ => unreachable!(),
                    }
                }
                TData::B(out)
            }
        })
    }

    /// `from`, `from_rows`, `eye`, `full` and `arange` for integers and `bool`.
    pub(super) fn int_ctor2(&mut self, kind: &str, vals: Vec<Value<'a>>, ek: ElemKind) -> R<'a, Value<'a>> {
        let usize_of = |v: &Value<'a>| -> R<'a, usize> {
            match v {
                Value::Int(n) if *n >= 0 => Ok(*n as usize),
                _ => panic("tensor sizes must be non-negative integers"),
            }
        };
        match kind {
            "tensor.from" => {
                let (s, a, b) = self.slice_of(&self.clone_deref(&vals[0]))?;
                let items: Vec<Value<'a>> = s.borrow()[a..b].to_vec();
                self.checked_len(&[items.len()])?;
                let n = items.len();
                Ok(Self::new_tensor(vec![n], self.data_from_values(items, ek)?, false))
            }
            "tensor.from_rows" => {
                let (s, a, b) = self.slice_of(&self.clone_deref(&vals[0]))?;
                let rows: Vec<Value<'a>> = s.borrow()[a..b].to_vec();
                let mut parsed: Vec<Vec<Value<'a>>> = Vec::new();
                for r in &rows {
                    let (rs, ra, rb) = self.slice_of(&self.clone_deref(r))?;
                    parsed.push(rs.borrow()[ra..rb].to_vec());
                }
                let cols = parsed.first().map(|r| r.len()).unwrap_or(0);
                if parsed.iter().any(|r| r.len() != cols) {
                    return panic("`from_rows` needs rows of the same length");
                }
                let nr = parsed.len();
                self.checked_len(&[nr, cols])?;
                let mut items = vec![Value::Unit; nr * cols];
                for (i, r) in parsed.iter().enumerate() {
                    for (j, x) in r.iter().enumerate() {
                        items[j * nr + i] = x.clone();
                    }
                }
                Ok(Self::new_tensor(vec![nr, cols], self.data_from_values(items, ek)?, false))
            }
            "tensor.eye" => {
                let n = usize_of(&vals[0])?;
                self.checked_len(&[n, n])?;
                let data = match ek {
                    ElemKind::Int(it) => {
                        let mut d = vec![0i128; n * n];
                        for i in 0..n {
                            d[i * n + i] = 1;
                        }
                        TData::I(it, d)
                    }
                    ElemKind::Bool => {
                        let mut d = vec![false; n * n];
                        for i in 0..n {
                            d[i * n + i] = true;
                        }
                        TData::B(d)
                    }
                };
                Ok(Self::new_tensor(vec![n, n], data, false))
            }
            "tensor.full" => {
                let v = self.elem_of_value(&vals[0], ek)?;
                let mut dims = Vec::new();
                for s in &vals[1..] {
                    dims.push(usize_of(s)?);
                }
                let n = self.checked_len(&dims)?;
                Ok(Self::new_tensor(dims, self.data_from_values(vec![v; n], ek)?, false))
            }
            "tensor.arange" => {
                let n = usize_of(&vals[0])?;
                self.checked_len(&[n])?;
                let ElemKind::Int(it) = ek else { return panic("`arange` needs a tensor of floats or integers") };
                let (_, hi) = super::eval::int_range(it);
                if n > 0 && (n as i128 - 1) > hi {
                    return panic(format!("integer overflow: {} does not fit in {}", hi + 1, it.name()));
                }
                Ok(Self::new_tensor(vec![n], TData::I(it, (0..n as i128).collect()), false))
            }
            other => panic(format!("`{other}` makes a tensor of floats"))
        }
    }

    // ------------------------------------------------------------ operators

    fn same_shape(a: &Tensor, b: &Tensor) -> R<'a, ()> {
        if a.dims != b.dims {
            return panic(format!("shape mismatch: {:?} and {:?}", a.dims, b.dims));
        }
        Ok(())
    }

    /// `+ - * / % & | ^` where the elements are integers or `bool`.
    pub(super) fn int_tensor_binop(&mut self, op: BinOp, l: &Value<'a>, r: &Value<'a>) -> R<'a, Value<'a>> {
        let (dims, data_l, data_r): (Vec<usize>, Option<TData>, Option<TData>);
        let (mut sl, mut sr) = (None, None);
        match (l, r) {
            (Value::Tensor(a), Value::Tensor(b)) => {
                let (a, b) = (a.borrow(), b.borrow());
                Self::same_shape(&a, &b)?;
                dims = a.dims.clone();
                data_l = Some(a.data.clone());
                data_r = Some(b.data.clone());
            }
            (Value::Tensor(a), s) => {
                let a = a.borrow();
                dims = a.dims.clone();
                data_l = Some(a.data.clone());
                data_r = None;
                sr = Some(self.deref(s.clone())?);
            }
            (s, Value::Tensor(b)) => {
                let b = b.borrow();
                dims = b.dims.clone();
                data_l = None;
                data_r = Some(b.data.clone());
                sl = Some(self.deref(s.clone())?);
            }
            _ => return panic("a tensor operation needs a tensor"),
        }
        let template = data_l.as_ref().or(data_r.as_ref()).unwrap();
        let n = Self::data_len(template);
        let get = |d: &Option<TData>, s: &Option<Value<'a>>, i: usize| -> Value<'a> {
            match (d, s) {
                (Some(d), _) => Self::tensor_elem(d, i),
                (None, Some(s)) => s.clone(),
                _ => unreachable!(),
            }
        };
        let data = match template {
            TData::I(it, _) => {
                let mut out = Vec::with_capacity(n);
                for i in 0..n {
                    let (Value::Int(a), Value::Int(b)) = (get(&data_l, &sl, i), get(&data_r, &sr, i)) else {
                        return panic("integer tensors combine with integers");
                    };
                    match self.int_op(op, a, b, *it)? {
                        Value::Int(v) => out.push(v),
                        _ => unreachable!(),
                    }
                }
                TData::I(*it, out)
            }
            TData::B(_) => {
                let mut out = Vec::with_capacity(n);
                for i in 0..n {
                    let (Value::Bool(a), Value::Bool(b)) = (get(&data_l, &sl, i), get(&data_r, &sr, i)) else {
                        return panic("tensors of bool combine with bools");
                    };
                    out.push(match op {
                        BinOp::BitAnd => a & b,
                        BinOp::BitOr => a | b,
                        BinOp::BitXor => a ^ b,
                        _ => return panic(format!("`{}` is not defined on tensors of bool", op.symbol())),
                    });
                }
                TData::B(out)
            }
            _ => return panic("not an integer tensor"),
        };
        Ok(Self::new_tensor(dims, data, false))
    }

    /// `a == b`, `a < 0.5` and the like on tensors: a tensor of `bool`.
    pub(super) fn tensor_compare(&mut self, op: BinOp, l: &Value<'a>, r: &Value<'a>) -> R<'a, Value<'a>> {
        let (dims, dl, dr, sl, sr): (Vec<usize>, Option<TData>, Option<TData>, Option<Value<'a>>, Option<Value<'a>>) = match (l, r) {
            (Value::Tensor(a), Value::Tensor(b)) => {
                let (a, b) = (a.borrow(), b.borrow());
                Self::same_shape(&a, &b)?;
                (a.dims.clone(), Some(a.data.clone()), Some(b.data.clone()), None, None)
            }
            (Value::Tensor(a), s) => {
                let a = a.borrow();
                (a.dims.clone(), Some(a.data.clone()), None, None, Some(self.deref(s.clone())?))
            }
            (s, Value::Tensor(b)) => {
                let b = b.borrow();
                (b.dims.clone(), None, Some(b.data.clone()), Some(self.deref(s.clone())?), None)
            }
            _ => return panic("a tensor comparison needs a tensor"),
        };
        let n = Self::data_len(dl.as_ref().or(dr.as_ref()).unwrap());
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let a = match (&dl, &sl) {
                (Some(d), _) => Self::tensor_elem(d, i),
                (None, Some(s)) => s.clone(),
                _ => unreachable!(),
            };
            let b = match (&dr, &sr) {
                (Some(d), _) => Self::tensor_elem(d, i),
                (None, Some(s)) => s.clone(),
                _ => unreachable!(),
            };
            out.push(match op {
                BinOp::Eq => self.values_eq(&a, &b)?,
                BinOp::Ne => !self.values_eq(&a, &b)?,
                _ => {
                    use std::cmp::Ordering::*;
                    match (op, self.values_cmp(&a, &b)?) {
                        (_, None) => false,
                        (BinOp::Lt, Some(o)) => o == Less,
                        (BinOp::Le, Some(o)) => o != Greater,
                        (BinOp::Gt, Some(o)) => o == Greater,
                        (_, Some(o)) => o != Less,
                    }
                }
            });
        }
        Ok(Self::new_tensor(dims, TData::B(out), false))
    }

    /// `!mask`
    pub(super) fn tensor_not(&mut self, t: &Tensor) -> R<'a, Value<'a>> {
        match &t.data {
            TData::B(d) => Ok(Self::new_tensor(t.dims.clone(), TData::B(d.iter().map(|b| !b).collect()), false)),
            _ => panic("`!` needs a tensor of bool"),
        }
    }

    /// `-t` for a tensor of integers (floats and complex numbers use the general path).
    pub(super) fn int_tensor_neg(&mut self, t: &Tensor) -> R<'a, Value<'a>> {
        let TData::I(it, d) = &t.data else { return panic("not an integer tensor") };
        let mut out = Vec::with_capacity(d.len());
        for &x in d {
            match self.fit(*it, -x)? {
                Value::Int(v) => out.push(v),
                _ => unreachable!(),
            }
        }
        Ok(Self::new_tensor(t.dims.clone(), TData::I(*it, out), false))
    }

    // ------------------------------------------------------------ methods

    /// The methods that need to know the element type beyond float and complex. Returns `None` for
    /// the ones the general code handles.
    pub(super) fn int_method(&mut self, kind: &str, t: &Tensor, args: &'a [Expr], f: &Frame<'a>) -> R<'a, Option<Value<'a>>> {
        let len = t.len();
        Ok(Some(match (kind, &t.data) {
            ("tensor.sum", TData::I(it, d)) => {
                let mut acc: i128 = 0;
                for &x in d {
                    match self.int_op(BinOp::Add, acc, x, *it)? {
                        Value::Int(v) => acc = v,
                        _ => unreachable!(),
                    }
                }
                Value::Int(acc)
            }
            ("tensor.min" | "tensor.max", TData::I(_, d)) => {
                let Some(&first) = d.first() else { return panic(format!("{} of an empty tensor", &kind[7..])) };
                let mut m = first;
                for &x in &d[1..] {
                    if (kind == "tensor.min" && x < m) || (kind == "tensor.max" && x > m) {
                        m = x;
                    }
                }
                Value::Int(m)
            }
            ("tensor.count", TData::B(d)) => Value::Int(d.iter().filter(|b| **b).count() as i128),
            ("tensor.any", TData::B(d)) => Value::Bool(d.iter().any(|b| *b)),
            ("tensor.all", TData::B(d)) => Value::Bool(d.iter().all(|b| *b)),
            ("tensor.select", _) => {
                let mask = self.eval(&args[0], f)?;
                let Value::Tensor(m) = self.deref(mask)? else { return panic("`select` needs a mask") };
                let m = m.borrow();
                if m.dims != t.dims {
                    return panic(format!("shape mismatch: cannot select from a tensor of shape {} with a mask of shape {}", shape_text_pub(&t.dims), shape_text_pub(&m.dims)));
                }
                let TData::B(bits) = &m.data else { return panic("`select` needs a mask of bool") };
                let pos: Vec<usize> = (0..len).filter(|&i| bits[i]).collect();
                Self::new_tensor(vec![pos.len()], Self::gather(&t.data, &pos), t.single)
            }
            ("tensor.to_f64", data) => {
                let out: Vec<f64> = match data {
                    TData::F(d) => d.clone(),
                    TData::I(_, d) => d.iter().map(|&x| x as f64).collect(),
                    TData::B(d) => d.iter().map(|&b| b as u8 as f64).collect(),
                    TData::C(_) => return panic("a complex tensor cannot be converted to f64"),
                };
                Self::new_tensor(t.dims.clone(), TData::F(out), false)
            }
            ("tensor.to_i64", data) => {
                let out: Vec<i128> = match data {
                    TData::F(d) => {
                        let (lo, hi) = super::eval::int_range(IntTy::I64);
                        d.iter().map(|&x| if x.is_nan() { 0 } else if x >= hi as f64 { hi } else if x <= lo as f64 { lo } else { x as i128 }).collect()
                    }
                    TData::I(_, d) => {
                        let (lo, hi) = super::eval::int_range(IntTy::I64);
                        for &x in d {
                            if x < lo || x > hi {
                                return panic(format!("integer overflow: {x} does not fit in i64"));
                            }
                        }
                        d.clone()
                    }
                    TData::B(d) => d.iter().map(|&b| b as i128).collect(),
                    TData::C(_) => return panic("a complex tensor cannot be converted to i64"),
                };
                Self::new_tensor(t.dims.clone(), TData::I(IntTy::I64, out), false)
            }
            ("tensor.map", TData::I(it, _)) => {
                let func = self.eval(&args[0], f)?;
                let mut out = Vec::with_capacity(len);
                for i in 0..len {
                    let r = self.call_value(func.clone(), vec![Self::tensor_elem(&t.data, i)])?;
                    match self.clone_deref(&r) {
                        Value::Int(x) => match self.fit(*it, x)? {
                            Value::Int(v) => out.push(v),
                            _ => unreachable!(),
                        },
                        other => return panic(format!("`map` expected the function to return the element type, got a {}", other.type_name())),
                    }
                }
                Self::new_tensor(t.dims.clone(), TData::I(*it, out), false)
            }
            ("tensor.map", TData::B(_)) => {
                let func = self.eval(&args[0], f)?;
                let mut out = Vec::with_capacity(len);
                for i in 0..len {
                    let r = self.call_value(func.clone(), vec![Self::tensor_elem(&t.data, i)])?;
                    match self.clone_deref(&r) {
                        Value::Bool(x) => out.push(x),
                        other => return panic(format!("`map` expected the function to return the element type, got a {}", other.type_name())),
                    }
                }
                Self::new_tensor(t.dims.clone(), TData::B(out), false)
            }
            _ => return Ok(None),
        }))
    }

    /// `a @ b` for integer tensors: the same loops as for floats, with checked arithmetic.
    pub(super) fn int_matmul(&mut self, a: &Tensor, b: &Tensor) -> R<'a, Value<'a>> {
        let (TData::I(it, x), TData::I(it2, y)) = (&a.data, &b.data) else { return panic("`@` needs tensors of numbers") };
        if it != it2 {
            return panic("`@` needs tensors of the same integer type");
        }
        let it = *it;
        match (a.dims.len(), b.dims.len()) {
            (2, 2) | (2, 1) => {
                let (m, k) = (a.dims[0], a.dims[1]);
                let (k2, n) = if b.dims.len() == 2 { (b.dims[0], b.dims[1]) } else { (b.dims[0], 1) };
                if k != k2 {
                    return panic(format!("shape mismatch for `@`: {:?} and {:?}", a.dims, b.dims));
                }
                if m.checked_mul(n).map_or(true, |t| t > crate::types::MAX_TENSOR_ELEMS) {
                    return panic(format!("a tensor can have at most {} elements", crate::types::MAX_TENSOR_ELEMS));
                }
                let mut c = vec![0i128; m * n];
                for j in 0..n {
                    for p in 0..k {
                        let bkj = y[p + j * k];
                        for i in 0..m {
                            let Value::Int(prod) = self.int_op(BinOp::Mul, x[p * m + i], bkj, it)? else { unreachable!() };
                            let Value::Int(sum) = self.int_op(BinOp::Add, c[j * m + i], prod, it)? else { unreachable!() };
                            c[j * m + i] = sum;
                        }
                    }
                }
                let dims = if b.dims.len() == 2 { vec![m, n] } else { vec![m] };
                Ok(Self::new_tensor(dims, TData::I(it, c), false))
            }
            (1, 1) => {
                if a.dims[0] != b.dims[0] {
                    return panic(format!("shape mismatch for `@`: {:?} and {:?}", a.dims, b.dims));
                }
                let mut acc = 0i128;
                for (p, q) in x.iter().zip(y) {
                    let Value::Int(prod) = self.int_op(BinOp::Mul, *p, *q, it)? else { unreachable!() };
                    let Value::Int(sum) = self.int_op(BinOp::Add, acc, prod, it)? else { unreachable!() };
                    acc = sum;
                }
                Ok(Value::Int(acc))
            }
            (p, q) => panic(format!("`@` is defined for vectors and matrices, not ranks {p} and {q}")),
        }
    }
}

/// The element type of an integer or `bool` tensor.
#[derive(Clone, Copy, Debug)]
pub enum ElemKind {
    Int(IntTy),
    Bool,
}

pub(super) fn shape_text_pub(dims: &[usize]) -> String {
    format!("[{}]", dims.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(", "))
}
