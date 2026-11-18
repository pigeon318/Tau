//! Run-time values.
//!
//! The borrow checker guarantees that mutable data is never aliased in a way that
//! matters, so aggregates are shared handles (`Rc<RefCell<..>>`):
//!  * reading a *place* by value copies the shell of tuples, arrays and structs
//!    (`copy_shell`), so `Copy` aggregates behave like values;
//!  * a `Vec`, `String`, map or tensor is never copied, so moving one is O(1);
//!  * an argument for a `mut` parameter is a `Ref` to the caller's place.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::{ClosureParam, Expr};
use crate::types::{AdtId, FnId, GenericId, Ty};

pub type Slot<'a> = Rc<RefCell<Vec<Value<'a>>>>;
pub type Subst = Rc<Vec<(GenericId, Ty)>>;

#[derive(Clone)]
pub enum Value<'a> {
    Unit,
    Bool(bool),
    /// Every integer type; the static type decides overflow behaviour.
    Int(i128),
    Float(f64),
    Complex(f64, f64),
    Char(char),
    Str(Rc<str>),
    String(Rc<RefCell<String>>),
    Tuple(Slot<'a>),
    /// A fixed-size array `[T; N]`.
    Array(Slot<'a>),
    Vec(Slot<'a>),
    /// A view of part of an array or vector: (storage, start, end).
    Slice(Slot<'a>, usize, usize),
    Struct(AdtId, Slot<'a>),
    Variant(AdtId, usize, Slot<'a>),
    Tensor(Rc<RefCell<Tensor>>),
    Map(Rc<RefCell<MapVal<'a>>>),
    Closure(Rc<Closure<'a>>),
    /// A function used as a value, with the types its generics were instantiated to.
    Func(FnId, Subst),
    /// A struct or variant constructor used as a function value.
    Ctor(AdtId, Option<usize>),
    /// A spawned task: its outcome and what it printed (tasks run to completion when
    /// spawned; their output appears when they are joined).
    Task(TaskCell<'a>),
    /// `a..b` (half open; `a..=b` is stored as `a..b+1`).
    Range(i128, i128),
    /// A raw pointer (an allocation).
    Ptr(Slot<'a>),
    /// A reference to a place (arguments for `mut` parameters, `mut` loop elements).
    Ref(RefTarget<'a>),
}

/// What a finished task left behind until it is joined.
pub type TaskCell<'a> = Rc<RefCell<Option<(Result<Value<'a>, String>, Rc<RefCell<Vec<u8>>>)>>>;

/// A writer that collects a task's output.
pub struct TaskSink(pub Rc<RefCell<Vec<u8>>>);

impl std::io::Write for TaskSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
pub enum RefTarget<'a> {
    Var(Rc<Env<'a>>, usize),
    Slot(Slot<'a>, usize),
    /// A map entry: the map, the key's hashable form, and the key as a value (to insert it).
    MapEntry(Rc<RefCell<MapVal<'a>>>, MapKey, Box<Value<'a>>),
    TensorElem(Rc<RefCell<Tensor>>, usize),
}

pub struct Closure<'a> {
    pub params: &'a [ClosureParam],
    pub body: &'a Expr,
    pub env: Rc<Env<'a>>,
    pub tyargs: Subst,
}

/// A scope of variables. Closures keep their defining scope alive, so captured
/// variables are shared with the code around them.
pub struct Env<'a> {
    pub vars: RefCell<Vec<(&'a str, Value<'a>)>>,
    pub parent: Option<Rc<Env<'a>>>,
}

impl<'a> Env<'a> {
    pub fn root() -> Rc<Env<'a>> {
        Rc::new(Env { vars: RefCell::new(Vec::new()), parent: None })
    }

    pub fn child(parent: &Rc<Env<'a>>) -> Rc<Env<'a>> {
        Rc::new(Env { vars: RefCell::new(Vec::new()), parent: Some(parent.clone()) })
    }

    pub fn declare(&self, name: &'a str, v: Value<'a>) {
        self.vars.borrow_mut().push((name, v));
    }

    /// The scope and index holding `name` (innermost first).
    pub fn find(self: &Rc<Self>, name: &str) -> Option<(Rc<Env<'a>>, usize)> {
        let mut cur: &Rc<Env<'a>> = self;
        loop {
            {
                let vars = cur.vars.borrow();
                for i in (0..vars.len()).rev() {
                    if vars[i].0 == name {
                        drop(vars);
                        return Some((cur.clone(), i));
                    }
                }
            }
            match &cur.parent {
                Some(p) => cur = p,
                None => return None,
            }
        }
    }

    /// The value of `name`, without cloning any scope.
    pub fn get(self: &Rc<Self>, name: &str) -> Option<Value<'a>> {
        let mut cur: &Rc<Env<'a>> = self;
        loop {
            {
                let vars = cur.vars.borrow();
                for i in (0..vars.len()).rev() {
                    if vars[i].0 == name {
                        return Some(vars[i].1.clone());
                    }
                }
            }
            match &cur.parent {
                Some(p) => cur = p,
                None => return None,
            }
        }
    }
}

/// A hashable form of a map key.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum MapKey {
    Int(i128),
    Str(String),
    Char(char),
    Bool(bool),
    Tuple(Vec<MapKey>),
}

#[derive(Default)]
pub struct MapVal<'a> {
    pub index: HashMap<MapKey, usize>,
    pub entries: Vec<(Value<'a>, Value<'a>)>,
}

#[derive(Clone, Debug)]
pub enum TData {
    F(Vec<f64>),
    C(Vec<(f64, f64)>),
}

/// A dense tensor, stored column-major (the first index varies fastest).
#[derive(Clone, Debug)]
pub struct Tensor {
    pub dims: Vec<usize>,
    pub data: TData,
    /// Round results to `f32` precision.
    pub single: bool,
}

impl Tensor {
    pub fn len(&self) -> usize {
        self.dims.iter().product()
    }

    /// Flat index of a multi-index (column-major), or `None` if out of range.
    pub fn flat(&self, idx: &[usize]) -> Option<usize> {
        if idx.len() != self.dims.len() {
            return None;
        }
        let mut stride = 1;
        let mut flat = 0;
        for (i, d) in idx.iter().zip(&self.dims) {
            if i >= d {
                return None;
            }
            flat += i * stride;
            stride *= d;
        }
        Some(flat)
    }
}

impl<'a> Value<'a> {
    pub fn unit() -> Value<'a> {
        Value::Unit
    }

    pub fn string(s: &str) -> Value<'a> {
        Value::String(Rc::new(RefCell::new(s.to_string())))
    }

    pub fn str(s: &str) -> Value<'a> {
        Value::Str(Rc::from(s))
    }

    pub fn slot(items: Vec<Value<'a>>) -> Slot<'a> {
        Rc::new(RefCell::new(items))
    }

    pub fn tuple(items: Vec<Value<'a>>) -> Value<'a> {
        Value::Tuple(Value::slot(items))
    }

    pub fn vec(items: Vec<Value<'a>>) -> Value<'a> {
        Value::Vec(Value::slot(items))
    }

    pub fn variant(adt: AdtId, idx: usize, fields: Vec<Value<'a>>) -> Value<'a> {
        Value::Variant(adt, idx, Value::slot(fields))
    }

    /// Duplicate the outer structure of tuples, arrays, structs and enum values, so
    /// that a value read from a place is independent of it. Heap objects that are
    /// moved rather than copied (vectors, strings, maps, tensors) stay shared.
    pub fn copy_shell(&self) -> Value<'a> {
        match self {
            Value::Tuple(s) => Value::Tuple(Value::slot(s.borrow().iter().map(|v| v.copy_shell()).collect())),
            Value::Array(s) => Value::Array(Value::slot(s.borrow().iter().map(|v| v.copy_shell()).collect())),
            Value::Struct(a, s) => Value::Struct(*a, Value::slot(s.borrow().iter().map(|v| v.copy_shell()).collect())),
            Value::Variant(a, i, s) => Value::Variant(*a, *i, Value::slot(s.borrow().iter().map(|v| v.copy_shell()).collect())),
            other => other.clone(),
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Unit => "()",
            Value::Bool(_) => "bool",
            Value::Int(_) => "integer",
            Value::Float(_) => "float",
            Value::Complex(..) => "complex",
            Value::Char(_) => "char",
            Value::Str(_) => "str",
            Value::String(_) => "String",
            Value::Tuple(_) => "tuple",
            Value::Array(_) => "array",
            Value::Vec(_) => "Vec",
            Value::Slice(..) => "slice",
            Value::Struct(..) => "struct",
            Value::Variant(..) => "enum value",
            Value::Tensor(_) => "tensor",
            Value::Map(_) => "Map",
            Value::Closure(_) | Value::Func(..) | Value::Ctor(..) => "function",
            Value::Task(_) => "Task",
            Value::Range(..) => "range",
            Value::Ptr(_) => "pointer",
            Value::Ref(_) => "reference",
        }
    }
}
