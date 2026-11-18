//! The borrow checker: moves, borrows, and the rules for tasks.
//!
//! It runs after type checking (it needs the types and resolutions the checker
//! recorded) and works in two steps: `lower` turns each function into a control-flow
//! graph of events, and `analysis` runs dataflow over that graph and reports errors.

mod analysis;
mod lower;
pub mod mir;

pub use lower::CaptureKind;
use std::collections::HashMap;

use crate::ast::*;
use crate::check::Results;
use crate::diag::Diagnostic;
use crate::program::Program;

/// Borrow-check every function in the user's module. The program must already have
/// type-checked without errors, and `results` must come from that same `user` module.
pub fn check(user: &Module, prog: &Program, results: &Results) -> Vec<Diagnostic> {
    let mut lw = lower_all(user, prog, results);
    let mut diags = std::mem::take(&mut lw.diags);
    for mir in &lw.done {
        diags.extend(analysis::analyze(mir));
    }
    diags.sort_by_key(|d| (d.span.start, d.span.end));
    // two rules can object to the same place; one message is enough
    diags.dedup_by(|a, b| a.span == b.span);
    diags
}

/// For every closure expression in the user's module, the variables it captures and how
/// (the native backend needs this to build the closure's environment).
pub fn capture_modes(user: &Module, prog: &Program, results: &Results) -> HashMap<crate::check::NodeKey, Vec<(String, CaptureKind)>> {
    lower_all(user, prog, results).captures
}

fn lower_all<'r>(user: &Module, prog: &'r Program, results: &'r Results) -> lower::Lowerer<'r> {
    let mut lw = lower::Lowerer::new(prog, results);
    for item in &user.items {
        match &item.kind {
            ItemKind::Fn(f) => lw.lower_fn_decl(f, false),
            ItemKind::Impl(i) => {
                for it in &i.items {
                    if let ItemKind::Fn(f) = &it.kind {
                        lw.lower_fn_decl(f, false);
                    }
                }
            }
            ItemKind::Trait(t) => {
                for it in &t.items {
                    if let ItemKind::Fn(f) = &it.kind {
                        lw.lower_fn_decl(f, false);
                    }
                }
            }
            _ => {}
        }
    }
    lw
}