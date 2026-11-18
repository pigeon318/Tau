//! Compact S-expression rendering of expressions, patterns, types and blocks.
//! Used by the parser tests (precedence is easy to read: `(+ a (* b c))`) and
//! by `tau parse --sexp`.

use crate::ast::*;

pub fn expr(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Int { value, suffix } => format!("{value}{}", suffix.as_deref().unwrap_or("")),
        ExprKind::Float { value, suffix } => {
            format!("{value:?}{}", suffix.as_deref().unwrap_or(""))
        }
        ExprKind::Imag(v) => format!("{v:?}i"),
        ExprKind::Bool(b) => b.to_string(),
        ExprKind::Char(c) => format!("{c:?}"),
        ExprKind::Str(pieces) => {
            let mut s = String::from("\"");
            for p in pieces {
                match p {
                    StrPiece::Lit(l) => s.push_str(l),
                    StrPiece::Interp(e) => {
                        s.push('{');
                        s.push_str(&expr(e));
                        s.push('}');
                    }
                }
            }
            s.push('"');
            s
        }
        ExprKind::Ident(n) => n.clone(),
        ExprKind::TypeApp { name, args } => {
            format!("{}<{}>", name.name, join_types(args))
        }
        ExprKind::Tuple(es) => list("tuple", es),
        ExprKind::Array(es) => list("array", es),
        ExprKind::ArrayRepeat { value, len } => format!("(repeat {} {})", expr(value), expr(len)),
        ExprKind::Unary { op, expr: x } => {
            let name = match op {
                UnOp::Neg => "neg",
                UnOp::Not => "!",
                UnOp::BitNot => "~",
            };
            format!("({name} {})", expr(x))
        }
        ExprKind::Binary { op, lhs, rhs } => {
            format!("({} {} {})", op.symbol(), expr(lhs), expr(rhs))
        }
        ExprKind::Assign { op, lhs, rhs } => {
            let sym = match op {
                None => "=".to_string(),
                Some(op) => format!("{}=", op.symbol()),
            };
            format!("({sym} {} {})", expr(lhs), expr(rhs))
        }
        ExprKind::Cast { expr: x, ty: t } => format!("(as {} {})", expr(x), ty(t)),
        ExprKind::Call { callee, args } => {
            let mut s = format!("(call {}", expr(callee));
            for a in args {
                s.push(' ');
                s.push_str(&expr(a));
            }
            s.push(')');
            s
        }
        ExprKind::MethodCall { recv, name, args } => {
            let mut s = format!("(method {} {}", expr(recv), name.name);
            for a in args {
                s.push(' ');
                s.push_str(&expr(a));
            }
            s.push(')');
            s
        }
        ExprKind::Field { base, name } => format!("(. {} {})", expr(base), name.name),
        ExprKind::Index { base, indices } => {
            let mut s = format!("(index {}", expr(base));
            for i in indices {
                s.push(' ');
                s.push_str(&expr(i));
            }
            s.push(')');
            s
        }
        ExprKind::Try(x) => format!("(? {})", expr(x)),
        ExprKind::Range {
            start,
            end,
            inclusive,
        } => {
            let op = if *inclusive { "..=" } else { ".." };
            let f = |o: &Option<Box<Expr>>| o.as_ref().map(|e| expr(e)).unwrap_or("_".into());
            format!("({op} {} {})", f(start), f(end))
        }
        ExprKind::Closure { params, ret, body } => {
            let ps: Vec<String> = params
                .iter()
                .map(|p| match &p.ty {
                    Some(t) => format!("{}:{}", pat(&p.pat), ty(t)),
                    None => pat(&p.pat),
                })
                .collect();
            let r = ret.as_ref().map(|t| format!(" -> {}", ty(t))).unwrap_or_default();
            format!("(closure ({}){r} {})", ps.join(" "), expr(body))
        }
        ExprKind::Block(b) => block(b),
        ExprKind::Unsafe(b) => format!("(unsafe {})", block(b)),
        ExprKind::If { cond, then, else_ } => match else_ {
            Some(e) => format!("(if {} {} {})", expr(cond), block(then), expr(e)),
            None => format!("(if {} {})", expr(cond), block(then)),
        },
        ExprKind::Match { scrutinee, arms } => {
            let mut s = format!("(match {}", expr(scrutinee));
            for a in arms {
                s.push(' ');
                match &a.guard {
                    Some(g) => s.push_str(&format!(
                        "(arm {} if {} => {})",
                        pat(&a.pat),
                        expr(g),
                        expr(&a.body)
                    )),
                    None => s.push_str(&format!("(arm {} => {})", pat(&a.pat), expr(&a.body))),
                }
            }
            s.push(')');
            s
        }
        ExprKind::Loop(b) => format!("(loop {})", block(b)),
        ExprKind::While { cond, body } => format!("(while {} {})", expr(cond), block(body)),
        ExprKind::For {
            par,
            pat: p,
            iter,
            guard,
            body,
        } => {
            let head = if *par { "par-for" } else { "for" };
            let g = guard
                .as_ref()
                .map(|g| format!(" (where {})", expr(g)))
                .unwrap_or_default();
            format!("({head} {} {}{g} {})", pat(p), expr(iter), block(body))
        }
        ExprKind::Return(v) => opt("return", v),
        ExprKind::Break(v) => opt("break", v),
        ExprKind::Continue => "(continue)".to_string(),
        ExprKind::MutBorrow(x) => format!("(mut {})", expr(x)),
        ExprKind::ParReduce {
            op,
            pat: p,
            iter,
            body,
        } => format!("(par-{} {} {} {})", op.name, pat(p), expr(iter), expr(body)),
        ExprKind::Spawn(b) => format!("(spawn {})", block(b)),
        ExprKind::On { target, body } => format!("(on {} {})", expr(target), block(body)),
    }
}

fn opt(name: &str, v: &Option<Box<Expr>>) -> String {
    match v {
        Some(e) => format!("({name} {})", expr(e)),
        None => format!("({name})"),
    }
}

fn list(head: &str, es: &[Expr]) -> String {
    let mut s = format!("({head}");
    for e in es {
        s.push(' ');
        s.push_str(&expr(e));
    }
    s.push(')');
    s
}

pub fn block(b: &Block) -> String {
    let mut s = String::from("(block");
    for st in &b.stmts {
        s.push(' ');
        s.push_str(&stmt(st));
    }
    s.push(')');
    s
}

pub fn stmt(s: &Stmt) -> String {
    match s {
        Stmt::Let { pat: p, ty: t, init, .. } => {
            let mut out = format!("(let {}", pat(p));
            if let Some(t) = t {
                out.push_str(&format!(" : {}", ty(t)));
            }
            if let Some(i) = init {
                out.push(' ');
                out.push_str(&expr(i));
            }
            out.push(')');
            out
        }
        Stmt::Expr(e) => expr(e),
        Stmt::Item(i) => format!("(item {})", item_kind(&i.kind)),
    }
}

fn item_kind(k: &ItemKind) -> &'static str {
    match k {
        ItemKind::Fn(_) => "fn",
        ItemKind::Struct(_) => "struct",
        ItemKind::Enum(_) => "enum",
        ItemKind::Trait(_) => "trait",
        ItemKind::Impl(_) => "impl",
        ItemKind::Use(_) => "use",
        ItemKind::Const(_) => "const",
        ItemKind::TypeAlias(_) => "type",
        ItemKind::Cluster(_) => "cluster",
    }
}

pub fn pat(p: &Pattern) -> String {
    match &p.kind {
        PatKind::Wild => "_".to_string(),
        PatKind::Ident { name, mutable } => {
            if *mutable {
                format!("(mut {name})")
            } else {
                name.clone()
            }
        }
        PatKind::Lit(e) => expr(e),
        PatKind::Range { lo, hi, inclusive } => {
            format!("({} {} {})", if *inclusive { "..=" } else { ".." }, expr(lo), expr(hi))
        }
        PatKind::Tuple(ps) => {
            let inner: Vec<String> = ps.iter().map(pat).collect();
            format!("(tuple {})", inner.join(" ")).replace("(tuple )", "(tuple)")
        }
        PatKind::Variant { path, args } => {
            let name: Vec<&str> = path.iter().map(|i| i.name.as_str()).collect();
            let inner: Vec<String> = args.iter().map(pat).collect();
            format!("({} {})", name.join("."), inner.join(" ")).replace(" )", ")")
        }
        PatKind::Path(path) => path
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>()
            .join("."),
        PatKind::Or(ps) => {
            let inner: Vec<String> = ps.iter().map(pat).collect();
            format!("(or {})", inner.join(" "))
        }
    }
}

fn join_types(ts: &[TypeExpr]) -> String {
    ts.iter().map(ty).collect::<Vec<_>>().join(", ")
}

pub fn ty(t: &TypeExpr) -> String {
    match &t.kind {
        TypeKind::Path { segments, args } => {
            let name: Vec<&str> = segments.iter().map(|i| i.name.as_str()).collect();
            if args.is_empty() {
                name.join(".")
            } else {
                format!("{}<{}>", name.join("."), join_types(args))
            }
        }
        TypeKind::Tuple(ts) => format!("({})", join_types(ts)),
        TypeKind::Array { elem, len } => format!("[{}; {}]", ty(elem), expr(len)),
        TypeKind::Slice(e) => format!("[{}]", ty(e)),
        TypeKind::Dims(ds) => {
            let inner: Vec<String> = ds
                .iter()
                .map(|d| match d {
                    Dim::Fixed(n) => n.to_string(),
                    Dim::Dynamic => "_".to_string(),
                    Dim::Named(i) => i.name.clone(),
                })
                .collect();
            format!("[{}]", inner.join(", "))
        }
        TypeKind::Fn { params, ret } => match ret {
            Some(r) => format!("fn({}) -> {}", join_types(params), ty(r)),
            None => format!("fn({})", join_types(params)),
        },
        TypeKind::Infer => "_".to_string(),
    }
}
