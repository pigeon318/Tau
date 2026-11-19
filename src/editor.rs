//! What editors need from the compiler: diagnostics with exact ranges, and the type of every
//! name for hovers. `tau check --json` prints this; a language server will serve the same data.
//!
//! Positions follow the Language Server Protocol: 0-based lines, and columns counted in UTF-16
//! code units, so an editor can use them as they are.

use crate::ast::*;
use crate::check::{CheckResult, NodeKey};
use crate::diag::Diagnostic;
use crate::span::Span;

/// 0-based line and UTF-16 column of a byte offset.
fn position(src: &str, offset: usize) -> (usize, usize) {
    let mut at = offset.min(src.len());
    while !src.is_char_boundary(at) {
        at -= 1;
    }
    let line_start = src[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line = src[..line_start].matches('\n').count();
    let col: usize = src[line_start..at].chars().map(|c| c.len_utf16()).sum();
    (line, col)
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn range_json(src: &str, span: Span) -> String {
    let (sl, sc) = position(src, span.start);
    // an empty span (an error at the end of the file) still gets one character to underline
    let end = if span.end > span.start { span.end } else { span.start };
    let (mut el, mut ec) = position(src, end);
    if (el, ec) == (sl, sc) {
        ec += 1;
        if sl != el {
            el = sl;
        }
    }
    format!("{{\"start\":{{\"line\":{sl},\"character\":{sc}}},\"end\":{{\"line\":{el},\"character\":{ec}}}}}")
}

fn diagnostic_json(src: &str, d: &Diagnostic) -> String {
    let mut s = format!("{{\"severity\":\"error\",\"message\":{},\"range\":{}", json_str(&d.message), range_json(src, d.span));
    if let Some(h) = &d.hint {
        s.push_str(&format!(",\"hint\":{}", json_str(h)));
    }
    if !d.notes.is_empty() {
        let notes: Vec<String> = d.notes.iter().map(|(sp, m)| format!("{{\"message\":{},\"range\":{}}}", json_str(m), range_json(src, *sp))).collect();
        s.push_str(&format!(",\"related\":[{}]", notes.join(",")));
    }
    s.push('}');
    s
}

/// A hover: the text to show for a range.
struct Hover {
    span: Span,
    text: String,
}

struct Collector<'a> {
    res: &'a CheckResult,
    hovers: Vec<Hover>,
}

impl<'a> Collector<'a> {
    fn ty_text(&self, ty: Option<&crate::types::Ty>) -> Option<String> {
        let t = ty?;
        if t.is_error() {
            return None;
        }
        let shown = self.res.show(t);
        if shown.contains("{error}") || shown.contains('_') && matches!(t, crate::types::Ty::Var(_)) {
            return None;
        }
        Some(shown)
    }

    fn fn_signature(&self, f: &FnDecl) -> Option<String> {
        let id = *self.res.results.fn_decl.get(&NodeKey::of(f))?;
        let def = self.res.program.fn_(id);
        let mut params = Vec::new();
        if let Some(m) = def.sig.self_mode {
            params.push(match m {
                ParamMode::Borrow => "self".to_string(),
                ParamMode::Mut => "mut self".to_string(),
                ParamMode::Own => "own self".to_string(),
            });
        }
        for p in &def.sig.params {
            let mode = match p.mode {
                ParamMode::Borrow => "",
                ParamMode::Mut => "mut ",
                ParamMode::Own => "own ",
            };
            params.push(format!("{}: {mode}{}", p.name, self.res.show(&p.ty)));
        }
        let ret = if def.sig.ret.is_unit() { String::new() } else { format!(" -> {}", self.res.show(&def.sig.ret)) };
        Some(format!("fn {}({}){}", def.name, params.join(", "), ret))
    }

    fn items(&mut self, items: &[Item]) {
        for it in items {
            match &it.kind {
                ItemKind::Fn(f) => self.func(f),
                ItemKind::Impl(i) => self.items(&i.items),
                ItemKind::Trait(t) => self.items(&t.items),
                ItemKind::Const(c) => self.expr(&c.value),
                _ => {}
            }
        }
    }

    fn func(&mut self, f: &FnDecl) {
        if let Some(sig) = self.fn_signature(f) {
            self.hovers.push(Hover { span: f.name.span, text: sig });
        }
        if let (Some(id), Some(_)) = (self.res.results.fn_decl.get(&NodeKey::of(f)), &f.body) {
            let def = self.res.program.fn_(*id);
            for (p, pd) in f.params.iter().filter(|p| p.name.name != "self").zip(&def.sig.params) {
                let mode = match pd.mode {
                    ParamMode::Borrow => "",
                    ParamMode::Mut => "mut ",
                    ParamMode::Own => "own ",
                };
                self.hovers.push(Hover { span: p.name.span, text: format!("{}: {mode}{}", p.name.name, self.res.show(&pd.ty)) });
            }
        }
        if let Some(b) = &f.body {
            self.block(b);
        }
    }

    fn block(&mut self, b: &Block) {
        for s in &b.stmts {
            match s {
                Stmt::Let { pat, init, .. } => {
                    self.pattern(pat);
                    if let Some(e) = init {
                        self.expr(e);
                    }
                }
                Stmt::Expr(e) => self.expr(e),
                Stmt::Item(it) => self.items(std::slice::from_ref(it)),
            }
        }
    }

    fn pattern(&mut self, p: &Pattern) {
        match &p.kind {
            PatKind::Ident { name, .. } => {
                if let Some(t) = self.ty_text(self.res.results.pat_ty.get(&NodeKey::of(p))) {
                    self.hovers.push(Hover { span: p.span, text: format!("{name}: {t}") });
                }
            }
            PatKind::Tuple(ps) | PatKind::Or(ps) => ps.iter().for_each(|x| self.pattern(x)),
            PatKind::Variant { args, .. } => args.iter().for_each(|x| self.pattern(x)),
            _ => {}
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Ident(name) => {
                if let Some(t) = self.ty_text(self.res.results.expr_ty.get(&NodeKey::of(e))) {
                    self.hovers.push(Hover { span: e.span, text: format!("{name}: {t}") });
                }
            }
            ExprKind::Field { base, name } => {
                if let Some(t) = self.ty_text(self.res.results.expr_ty.get(&NodeKey::of(e))) {
                    self.hovers.push(Hover { span: name.span, text: format!("{}: {t}", name.name) });
                }
                self.expr(base);
            }
            ExprKind::MethodCall { recv, name, args } => {
                if let Some(crate::check::Res::Fn(fid, _)) = self.res.results.expr_res.get(&NodeKey::of(e)) {
                    let def = self.res.program.fn_(*fid);
                    let ret = if def.sig.ret.is_unit() { String::new() } else { format!(" -> {}", self.res.show(&def.sig.ret)) };
                    let params: Vec<String> = def.sig.params.iter().map(|p| format!("{}: {}", p.name, self.res.show(&p.ty))).collect();
                    self.hovers.push(Hover { span: name.span, text: format!("fn {}({}){}", def.name, params.join(", "), ret) });
                }
                self.expr(recv);
                args.iter().for_each(|a| self.expr(a));
            }
            ExprKind::Closure { params, body, .. } => {
                for p in params {
                    self.pattern(&p.pat);
                }
                self.expr(body);
            }
            ExprKind::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                for a in arms {
                    self.pattern(&a.pat);
                    if let Some(g) = &a.guard {
                        self.expr(g);
                    }
                    self.expr(&a.body);
                }
            }
            ExprKind::For { pat, iter, guard, body, .. } => {
                self.pattern(pat);
                self.expr(iter);
                if let Some(g) = guard {
                    self.expr(g);
                }
                self.block(body);
            }
            ExprKind::ParReduce { pat, iter, body, .. } => {
                self.pattern(pat);
                self.expr(iter);
                self.expr(body);
            }
            ExprKind::Block(b) | ExprKind::Unsafe(b) | ExprKind::Loop(b) | ExprKind::Spawn(b) => self.block(b),
            ExprKind::If { cond, then, else_ } => {
                self.expr(cond);
                self.block(then);
                if let Some(x) = else_ {
                    self.expr(x);
                }
            }
            ExprKind::While { cond, body } => {
                self.expr(cond);
                self.block(body);
            }
            ExprKind::On { target, body } => {
                self.expr(target);
                self.block(body);
            }
            ExprKind::Str(ps) => {
                for p in ps {
                    if let StrPiece::Interp(x) = p {
                        self.expr(x);
                    }
                }
            }
            ExprKind::Tuple(es) | ExprKind::Array(es) => es.iter().for_each(|x| self.expr(x)),
            ExprKind::ArrayRepeat { value, len } => {
                self.expr(value);
                self.expr(len);
            }
            ExprKind::Unary { expr, .. } | ExprKind::Cast { expr, .. } | ExprKind::Try(expr) | ExprKind::MutBorrow(expr) => self.expr(expr),
            ExprKind::Binary { lhs, rhs, .. } | ExprKind::Assign { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::Call { callee, args } => {
                self.expr(callee);
                args.iter().for_each(|a| self.expr(a));
            }
            ExprKind::Index { base, indices } => {
                self.expr(base);
                indices.iter().for_each(|a| self.expr(a));
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(x) = start {
                    self.expr(x);
                }
                if let Some(x) = end {
                    self.expr(x);
                }
            }
            ExprKind::Return(v) | ExprKind::Break(v) => {
                if let Some(x) = v {
                    self.expr(x);
                }
            }
            _ => {}
        }
    }
}

/// Check `src` and describe the result as JSON for an editor:
///
/// ```text
/// {"ok": true, "diagnostics": [ {"severity", "message", "range", "hint"?, "related"?} ],
///  "hovers": [ {"range", "text"} ]}
/// ```
pub fn check_json(src: &str) -> String {
    let mut diags: Vec<String> = Vec::new();
    let mut hovers: Vec<String> = Vec::new();
    let user = match crate::parse_module(src) {
        Ok(m) => Some(m),
        Err(d) => {
            diags.push(diagnostic_json(src, &d));
            None
        }
    };
    if let Some(user) = &user {
        let lib = crate::prelude::load();
        let result = crate::analyze(&lib, user);
        for d in &result.diagnostics {
            diags.push(diagnostic_json(src, d));
        }
        let mut c = Collector { res: &result, hovers: Vec::new() };
        c.items(&user.items);
        c.hovers.sort_by_key(|h| (h.span.start, h.span.end));
        c.hovers.dedup_by(|a, b| a.span == b.span);
        for h in &c.hovers {
            hovers.push(format!("{{\"range\":{},\"text\":{}}}", range_json(src, h.span), json_str(&h.text)));
        }
    }
    format!("{{\"ok\":{},\"diagnostics\":[{}],\"hovers\":[{}]}}", diags.is_empty(), diags.join(","), hovers.join(","))
}
