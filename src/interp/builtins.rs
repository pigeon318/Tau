//! Built-in functions: the ones declared without a body in `src/prelude/*.tau`.

use std::cell::RefCell;
use std::io::BufRead;
use std::rc::Rc;

use super::*;

impl<'a> Interp<'a> {
    fn i64_of(&self, v: &Value<'a>) -> R<'a, i64> {
        match v {
            Value::Int(i) => Ok(*i as i64),
            other => panic(format!("expected an integer, found a {}", other.type_name())),
        }
    }

    fn f64_of(&self, v: &Value<'a>) -> R<'a, f64> {
        match v {
            Value::Float(x) => Ok(*x),
            Value::Int(i) => Ok(*i as f64),
            other => panic(format!("expected a number, found a {}", other.type_name())),
        }
    }

    /// Any vector, array or slice, as (storage, start, end).
    pub fn slice_of(&self, v: &Value<'a>) -> R<'a, (Slot<'a>, usize, usize)> {
        match v {
            Value::Vec(s) | Value::Array(s) | Value::Ptr(s) => {
                let n = s.borrow().len();
                Ok((s.clone(), 0, n))
            }
            Value::Slice(s, a, b) => Ok((s.clone(), *a, *b)),
            other => panic(format!("expected a vector or slice, found a {}", other.type_name())),
        }
    }

    fn ok_value(&self, v: Value<'a>) -> Value<'a> {
        Value::variant(self.result, 0, vec![v])
    }

    fn err_value(&self, v: Value<'a>) -> Value<'a> {
        Value::variant(self.result, 1, vec![v])
    }

    fn io_error(&self, msg: String) -> Value<'a> {
        let adt = self.prog.find_adt("IoError").expect("prelude defines IoError");
        self.err_value(Value::Struct(adt, Value::slot(vec![Value::string(&msg)])))
    }

    fn some(&self, v: Value<'a>) -> Value<'a> {
        Value::variant(self.option, 0, vec![v])
    }

    fn none(&self) -> Value<'a> {
        Value::variant(self.option, 1, vec![])
    }

    pub fn next_random(&mut self) -> f64 {
        // xorshift64*, fixed seed: runs are reproducible
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (r >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn call_builtin(&mut self, key: &str, args: Vec<Value<'a>>, ret: &Ty, subst: &[(GenericId, Ty)]) -> R<'a, Value<'a>> {
        let mut dargs = Vec::with_capacity(args.len());
        for a in args {
            dargs.push(self.deref(a)?);
        }
        let a = dargs;
        let single = matches!(ret, Ty::Float(FloatTy::F32));
        let fl = |x: f64| Value::Float(if single { x as f32 as f64 } else { x });
        match key {
            // ---- maths
            "sqrt" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "exp" | "ln" | "log10" | "floor" | "ceil" | "round" => {
                let x = self.f64_of(&a[0])?;
                Ok(fl(match key {
                    "sqrt" => x.sqrt(),
                    "sin" => x.sin(),
                    "cos" => x.cos(),
                    "tan" => x.tan(),
                    "asin" => x.asin(),
                    "acos" => x.acos(),
                    "atan" => x.atan(),
                    "exp" => x.exp(),
                    "ln" => x.ln(),
                    "log10" => x.log10(),
                    "floor" => x.floor(),
                    "ceil" => x.ceil(),
                    _ => x.round(),
                }))
            }
            "atan2" => Ok(fl(self.f64_of(&a[0])?.atan2(self.f64_of(&a[1])?))),
            "abs" => Ok(match &a[0] {
                Value::Int(i) => {
                    let r = i.abs();
                    if let Ty::Int(it) = ret {
                        let (_, hi) = super::eval::int_range(*it);
                        if r > hi {
                            return panic(format!("integer overflow: {r} does not fit in {}", it.name()));
                        }
                    }
                    Value::Int(r)
                }
                Value::Float(x) => Value::Float(x.abs()),
                Value::Complex(p, q) => Value::Complex(p.hypot(*q), 0.0),
                other => return panic(format!("cannot take abs of a {}", other.type_name())),
            }),
            "zero" => self.default_value(ret),
            "clone" => Ok(clone_value(&a[0])),
            "one" => Ok(match ret {
                Ty::Float(_) => Value::Float(1.0),
                Ty::Complex(..) => Value::Complex(1.0, 0.0),
                _ => Value::Int(1),
            }),
            "min" | "max" => {
                let c = self.values_cmp(&a[0], &a[1])?;
                let first_smaller = matches!(c, Some(std::cmp::Ordering::Less));
                Ok(if (key == "min") == first_smaller || c == Some(std::cmp::Ordering::Equal) { a[0].clone() } else { a[1].clone() })
            }
            "sum" | "slice::sum" => {
                let (s, lo, hi) = self.slice_of(&a[0])?;
                let items: Vec<Value<'a>> = s.borrow()[lo..hi].to_vec();
                let mut acc = self.default_value(ret)?;
                for x in items {
                    acc = self.bin_values(crate::ast::BinOp::Add, acc, x, ret)?;
                }
                Ok(acc)
            }
            "trace" => self.tensor_trace(&a[0]),

            // ---- output and checks
            "print" => {
                // `print<T>`: the type argument says how to format the value
                let text = match subst.first() {
                    Some((_, ty)) => self.display_ty(&a[0], ty)?,
                    None => self.display(&a[0])?,
                };
                let _ = writeln!(self.out, "{text}");
                Ok(Value::Unit)
            }
            "assert" => match &a[0] {
                Value::Bool(true) => Ok(Value::Unit),
                _ => panic("assertion failed"),
            },
            "assert_eq" => {
                if self.values_eq(&a[0], &a[1])? {
                    Ok(Value::Unit)
                } else {
                    let (l, r) = (self.display(&a[0])?, self.display(&a[1])?);
                    panic(format!("assertion failed: left != right\n  left: {l}\n right: {r}"))
                }
            }
            "panic" => panic(self.text_of(&a[0])),

            // ---- memory
            "alloc" => {
                let n = match &a[0] {
                    Value::Int(n) if *n >= 0 => *n as usize,
                    _ => return panic("alloc needs a non-negative size"),
                };
                let elem = match ret {
                    Ty::Adt(_, args) if args.len() == 1 => args[0].clone(),
                    _ => Ty::Int(IntTy::U8),
                };
                let zero = self.default_value(&elem)?;
                Ok(Value::Ptr(Value::slot((0..n).map(|_| zero.copy_shell()).collect())))
            }
            "Ptr::index" => {
                let (s, _, _) = self.slice_of(&a[0])?;
                match &a[1] {
                    Value::Int(i) if *i >= 0 && (*i as usize) < s.borrow().len() => Ok(s.borrow()[*i as usize].clone()),
                    _ => panic("pointer index out of bounds"),
                }
            }
            "par_chunks" => {
                let (s, lo, hi) = self.slice_of(&a[0])?;
                let size = match &a[1] {
                    Value::Int(n) if *n > 0 => *n as usize,
                    _ => return panic("par_chunks needs a positive chunk size"),
                };
                let mut chunks = Vec::new();
                let mut i = lo;
                while i < hi {
                    let j = (i + size).min(hi);
                    chunks.push(Value::Slice(s.clone(), i, j));
                    i = j;
                }
                Ok(Value::vec(chunks))
            }

            // ---- Vec and slices
            "Vec::new" => Ok(Value::vec(vec![])),
            "Vec::push" => match &a[0] {
                Value::Vec(s) => {
                    s.borrow_mut().push(a[1].copy_shell());
                    Ok(Value::Unit)
                }
                other => panic(format!("push on a {}", other.type_name())),
            },
            "Vec::pop" => match &a[0] {
                Value::Vec(s) => {
                    let v = s.borrow_mut().pop();
                    Ok(match v {
                        Some(x) => self.some(x),
                        None => self.none(),
                    })
                }
                other => panic(format!("pop on a {}", other.type_name())),
            },
            "Vec::from_raw" => {
                let (s, _, _) = self.slice_of(&a[0])?;
                let n = match &a[1] {
                    Value::Int(n) if *n >= 0 => *n as usize,
                    _ => return panic("from_raw needs a non-negative length"),
                };
                let mut items = s.borrow().clone();
                items.truncate(n);
                Ok(Value::vec(items))
            }
            "slice::map" => {
                let (s, lo, hi) = self.slice_of(&a[0])?;
                let items: Vec<Value<'a>> = s.borrow()[lo..hi].to_vec();
                let mut out = Vec::with_capacity(items.len());
                for x in items {
                    out.push(self.call_value(a[1].clone(), vec![x])?);
                }
                Ok(Value::vec(out))
            }
            "slice::filter" => {
                let (s, lo, hi) = self.slice_of(&a[0])?;
                let items: Vec<Value<'a>> = s.borrow()[lo..hi].to_vec();
                let mut out = Vec::new();
                for x in items {
                    if matches!(self.call_value(a[1].clone(), vec![x.clone()])?, Value::Bool(true)) {
                        out.push(x);
                    }
                }
                Ok(Value::vec(out))
            }
            "slice::to_vec" => {
                let (s, lo, hi) = self.slice_of(&a[0])?;
                let items: Vec<Value<'a>> = s.borrow()[lo..hi].iter().map(|v| v.copy_shell()).collect();
                Ok(Value::vec(items))
            }
            "slice::swap" => {
                let (s, lo, hi) = self.slice_of(&a[0])?;
                let len = hi - lo;
                let idx = |v: &Value<'a>| -> R<'a, usize> {
                    match v {
                        Value::Int(i) if *i >= 0 && (*i as usize) < len => Ok(lo + *i as usize),
                        Value::Int(i) => panic(format!("index out of bounds: the length is {len} but the index is {i}")),
                        _ => panic("swap needs integer positions"),
                    }
                };
                let (i, j) = (idx(&a[1])?, idx(&a[2])?);
                s.borrow_mut().swap(i, j);
                Ok(Value::Unit)
            }
            "slice::is_empty" => {
                let (_, lo, hi) = self.slice_of(&a[0])?;
                Ok(Value::Bool(lo == hi))
            }

            // ---- tasks
            "Task::join" => match &a[0] {
                Value::Task(cell) => {
                    let outcome = cell.borrow_mut().take();
                    if let Some((_, sink)) = &outcome {
                        let _ = self.out.write_all(&sink.borrow());
                    }
                    match outcome.map(|(r, _)| r) {
                        Some(Ok(v)) => Ok(self.ok_value(v)),
                        Some(Err(_)) => {
                            let adt = self.prog.find_adt("TaskPanicked").expect("prelude defines TaskPanicked");
                            Ok(self.err_value(Value::Struct(adt, Value::slot(vec![]))))
                        }
                        None => panic("a task was joined twice"),
                    }
                }
                other => panic(format!("join on a {}", other.type_name())),
            },

            // ---- strings
            "String::new" => Ok(Value::string("")),
            "String::from" | "str::to_string" => Ok(Value::string(&self.text_of(&a[0]))),
            "String::push_str" => match &a[0] {
                Value::String(s) => {
                    s.borrow_mut().push_str(&self.text_of(&a[1]));
                    Ok(Value::Unit)
                }
                other => panic(format!("push_str on a {}", other.type_name())),
            },
            "str::byte_at" => {
                let text = self.text_of(&a[0]);
                match &a[1] {
                    Value::Int(i) if *i >= 0 && (*i as usize) < text.len() => Ok(Value::Int(text.as_bytes()[*i as usize] as i128)),
                    Value::Int(i) => panic(format!("index out of bounds: the length is {} but the index is {i}", text.len())),
                    _ => panic("byte_at needs an integer position"),
                }
            }
            "String::push" => match (&a[0], &a[1]) {
                (Value::String(s), Value::Char(c)) => {
                    s.borrow_mut().push(*c);
                    Ok(Value::Unit)
                }
                _ => panic("push needs a string and a character"),
            },
            "char_from_u32" => {
                let Value::Int(n) = &a[0] else { return panic("char_from_u32 needs an integer") };
                Ok(match u32::try_from(*n).ok().and_then(char::from_u32) {
                    Some(c) => self.some(Value::Char(c)),
                    None => self.none(),
                })
            }
            "str::parse_i64" => {
                let text = self.text_of(&a[0]);
                Ok(match parse_i64_text(&text) {
                    Ok(v) => self.ok_value(Value::Int(v as i128)),
                    Err(msg) => self.err_value(Value::string(&msg)),
                })
            }
            "str::parse_f64" => {
                let text = self.text_of(&a[0]);
                Ok(match parse_f64_text(&text) {
                    Ok(v) => self.ok_value(Value::Float(v)),
                    Err(msg) => self.err_value(Value::string(&msg)),
                })
            }
            "str::lowercase" => Ok(Value::string(&self.text_of(&a[0]).to_lowercase())),
            "str::uppercase" => Ok(Value::string(&self.text_of(&a[0]).to_uppercase())),
            "str::split_whitespace" => {
                let text = self.text_of(&a[0]);
                Ok(Value::vec(text.split_whitespace().map(Value::str).collect()))
            }

            // ---- files
            "std.fs::read" => Ok(match std::fs::read_to_string(self.text_of(&a[0])) {
                Ok(s) => self.ok_value(Value::string(&s)),
                Err(e) => self.io_error(e.to_string()),
            }),
            "std.fs::write" => Ok(match std::fs::write(self.text_of(&a[0]), self.text_of(&a[1])) {
                Ok(()) => self.ok_value(Value::Unit),
                Err(e) => self.io_error(e.to_string()),
            }),
            "std.fs::exists" => Ok(Value::Bool(std::path::Path::new(&self.text_of(&a[0])).exists())),
            "std.io::read_line" => {
                let mut line = String::new();
                Ok(match std::io::stdin().lock().read_line(&mut line) {
                    Ok(_) => self.ok_value(Value::string(line.trim_end_matches(['\n', '\r']))),
                    Err(e) => self.io_error(e.to_string()),
                })
            }

            // ---- time
            "std.time::monotonic_ns" => Ok(Value::Int(clock::monotonic_ns() as i128)),
            "std.time::unix_ns" => Ok(Value::Int(clock::unix_ns() as i128)),
            "std.time::sleep" => {
                let secs = self.f64_of(&a[0])?;
                if let Some(m) = clock::bad_sleep(secs) {
                    return panic(m);
                }
                clock::sleep(secs);
                Ok(Value::Unit)
            }
            "std.time::civil" => Ok(Value::tuple(clock::civil(self.i64_of(&a[0])?).iter().map(|&x| Value::Int(x as i128)).collect())),
            "std.time::weekday" => Ok(Value::Int(clock::weekday(self.i64_of(&a[0])?) as i128)),
            "std.time::from_civil" => {
                let mut n = [0i64; 6];
                for (slot, v) in n.iter_mut().zip(&a) {
                    *slot = self.i64_of(v)?;
                }
                match clock::from_civil(n[0], n[1], n[2], n[3], n[4], n[5]) {
                    Some(t) => Ok(Value::Int(t as i128)),
                    None => panic("invalid date or time"),
                }
            }
            "std.time::format_iso" => Ok(Value::string(&clock::iso(self.i64_of(&a[0])?))),
            "std.time::format_duration" => Ok(Value::string(&clock::duration(self.i64_of(&a[0])?))),

            // ---- boxes
            "Box::new" => Ok(Value::Box(Value::slot(vec![a[0].clone()]))),
            "Box::take" => match &a[0] {
                Value::Box(s) => {
                    let v = s.borrow()[0].clone();
                    Ok(v)
                }
                other => panic(format!("take on a {}", other.type_name())),
            },

            // ---- shared values and mutexes
            "Shared::new" => Ok(Value::Shared(Value::slot(vec![a[0].clone()]))),
            "Mutex::new" => Ok(Value::Mutex(Value::slot(vec![a[0].clone(), Value::Bool(false)]))),
            "Mutex::get" | "Mutex::set" | "Mutex::update" | "Mutex::with" | "Mutex::into_inner" => {
                let Value::Mutex(s) = &a[0] else { return panic(format!("a mutex method on a {}", a[0].type_name())) };
                let s = s.clone();
                if matches!(s.borrow()[1], Value::Bool(true)) {
                    return panic("deadlock: the mutex is already locked by this thread");
                }
                let cur = s.borrow()[0].clone();
                match key {
                    "Mutex::get" => Ok(clone_value(&cur)),
                    "Mutex::into_inner" => Ok(cur),
                    "Mutex::set" => {
                        s.borrow_mut()[0] = a[1].clone();
                        Ok(Value::Unit)
                    }
                    _ => {
                        // the function runs with the mutex locked (using it again inside would wait for itself)
                        s.borrow_mut()[1] = Value::Bool(true);
                        let r = self.call_value(a[1].clone(), vec![cur]);
                        s.borrow_mut()[1] = Value::Bool(false);
                        let r = r?;
                        if key == "Mutex::update" {
                            s.borrow_mut()[0] = r;
                            Ok(Value::Unit)
                        } else {
                            Ok(r)
                        }
                    }
                }
            }

            // ---- maps
            "Map::new" => Ok(Value::Map(Rc::new(RefCell::new(MapVal::default())))),
            "Map::contains" => match &a[0] {
                Value::Map(m) => {
                    let k = self.map_key(&a[1])?;
                    let has = m.borrow().index.contains_key(&k);
                    Ok(Value::Bool(has))
                }
                other => panic(format!("contains on a {}", other.type_name())),
            },
            "Map::get" => match &a[0] {
                Value::Map(m) => {
                    let k = self.map_key(&a[1])?;
                    let found = m.borrow().index.get(&k).map(|i| clone_value(&m.borrow().entries[*i].1));
                    Ok(match found {
                        Some(v) => self.some(v),
                        None => self.none(),
                    })
                }
                other => panic(format!("get on a {}", other.type_name())),
            },
            "Map::remove" => match &a[0] {
                Value::Map(m) => {
                    let k = self.map_key(&a[1])?;
                    let mut mm = m.borrow_mut();
                    match mm.index.remove(&k) {
                        Some(i) => {
                            let (_, v) = mm.entries.remove(i);
                            for idx in mm.index.values_mut() {
                                if *idx > i {
                                    *idx -= 1;
                                }
                            }
                            drop(mm);
                            Ok(self.some(v))
                        }
                        None => {
                            drop(mm);
                            Ok(self.none())
                        }
                    }
                }
                other => panic(format!("remove on a {}", other.type_name())),
            },
            "Map::keys" | "Map::values" => match &a[0] {
                Value::Map(m) => {
                    let items: Vec<Value<'a>> = m.borrow().entries.iter().map(|(k, v)| clone_value(if key == "Map::keys" { k } else { v })).collect();
                    Ok(Value::vec(items))
                }
                other => panic(format!("{key} on a {}", other.type_name())),
            },
            "Map::index" => match &a[0] {
                Value::Map(m) => {
                    let k = self.map_key(&a[1])?;
                    let m = m.borrow();
                    match m.index.get(&k) {
                        Some(i) => Ok(m.entries[*i].1.clone()),
                        None => panic("key not found in map"),
                    }
                }
                other => panic(format!("index on a {}", other.type_name())),
            },
            "Map::top" => match &a[0] {
                Value::Map(m) => {
                    let n = match &a[1] {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        _ => return panic("top needs a non-negative count"),
                    };
                    let mut entries: Vec<(Value<'a>, Value<'a>)> = m.borrow().entries.clone();
                    // highest value first; equal values by key, so the order is stable
                    let mut failed = None;
                    let mut sorted: Vec<usize> = (0..entries.len()).collect();
                    sorted.sort_by(|&i, &j| {
                        let by_value = self.values_cmp(&entries[j].1, &entries[i].1);
                        let by_key = self.values_cmp(&entries[i].0, &entries[j].0);
                        match (by_value, by_key) {
                            (Ok(Some(std::cmp::Ordering::Equal)), Ok(k)) => k.unwrap_or(std::cmp::Ordering::Equal),
                            (Ok(Some(v)), _) => v,
                            (Err(e), _) | (_, Err(e)) => {
                                failed = Some(e);
                                std::cmp::Ordering::Equal
                            }
                            _ => std::cmp::Ordering::Equal,
                        }
                    });
                    if let Some(e) = failed {
                        return Err(e);
                    }
                    let out: Vec<Value<'a>> = sorted
                        .into_iter()
                        .take(n)
                        .map(|i| Value::tuple(vec![std::mem::replace(&mut entries[i].0, Value::Unit), std::mem::replace(&mut entries[i].1, Value::Unit)]))
                        .collect();
                    Ok(Value::vec(out))
                }
                other => panic(format!("top on a {}", other.type_name())),
            },

            // ---- linear algebra and FFT
            "math.linalg::eig" | "math.linalg::det" | "math.linalg::inv" | "math.linalg::transpose" | "math.fft::fft" | "math.fft::ifft" => {
                self.linalg(key, &a)
            }

            other => {
                let _ = subst;
                panic(format!("the built-in `{other}` is not implemented yet"))
            }
        }
    }
}

/// A copy that shares nothing with the original (`Clone`): vectors, strings, maps, tensors and boxes
/// are duplicated too.
pub(super) fn clone_value<'a>(v: &Value<'a>) -> Value<'a> {
    let all = |s: &Slot<'a>| -> Vec<Value<'a>> { s.borrow().iter().map(clone_value).collect() };
    match v {
        Value::Tuple(s) => Value::Tuple(Value::slot(all(s))),
        Value::Array(s) => Value::Array(Value::slot(all(s))),
        Value::Vec(s) => Value::Vec(Value::slot(all(s))),
        Value::Box(s) => Value::Box(Value::slot(all(s))),
        // another handle to the same value
        Value::Shared(s) => Value::Shared(s.clone()),
        Value::Struct(a, s) => Value::Struct(*a, Value::slot(all(s))),
        Value::Variant(a, i, s) => Value::Variant(*a, *i, Value::slot(all(s))),
        Value::String(s) => Value::String(Rc::new(RefCell::new(s.borrow().clone()))),
        Value::Tensor(t) => Value::Tensor(Rc::new(RefCell::new(t.borrow().clone()))),
        Value::Map(m) => {
            let m = m.borrow();
            Value::Map(Rc::new(RefCell::new(MapVal {
                index: m.index.clone(),
                entries: m.entries.iter().map(|(k, x)| (clone_value(k), clone_value(x))).collect(),
            })))
        }
        other => other.clone(),
    }
}

/// `s[a..b]` on text: the same checks and messages as slicing a vector, plus character boundaries.
pub(super) fn str_slice<'a>(text: &str, a: i128, b: i128) -> R<'a, Value<'a>> {
    let len = text.len() as i128;
    let b = b.min(len);
    if a < 0 || a > b || b > len {
        return panic(format!("slice {a}..{b} is out of range for length {len}"));
    }
    for i in [a, b] {
        if !text.is_char_boundary(i as usize) {
            return panic(format!("byte index {i} is not a character boundary"));
        }
    }
    Ok(Value::str(&text[a as usize..b as usize]))
}

/// An optional sign and decimal digits.
pub(super) fn parse_i64_text(text: &str) -> Result<i64, String> {
    let bad = || format!("cannot parse \"{text}\" as an integer");
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    text.parse::<i64>().map_err(|_| format!("cannot parse \"{text}\" as an integer: it does not fit in i64"))
}

/// An optional sign, digits with an optional `.`, and an optional exponent.
pub(super) fn parse_f64_text(text: &str) -> Result<f64, String> {
    let bad = || format!("cannot parse \"{text}\" as a number");
    let b = text.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let (mut before, mut after) = (0, 0);
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        before += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            after += 1;
        }
    }
    if before + after == 0 {
        return Err(bad());
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let mut e = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            e += 1;
        }
        if e == 0 {
            return Err(bad());
        }
    }
    if i != b.len() {
        return Err(bad());
    }
    text.parse::<f64>().map_err(|_| bad())
}
