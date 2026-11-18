//! Implements the reduction passes and their switches.

use std::collections::HashSet;

use spass_lexer::Span;
use spass_parser::ast::{BinOp, Block, Expr, Stmt, Type, UnaryOp};

use crate::const_eval::{self, ConstValue};

// ============================================================================
// Type classes
// ============================================================================

/// The type class of a value.
///
/// The reducer needs to know that a replacement has the same type as the
/// binding it replaces: the emitter types a local from its declaration
/// or, for an unannotated one, from its initialiser, and a use site
/// expects the local's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// A signed 32-bit integer.
    Int,
    /// An unsigned 32-bit integer.
    Uint,
    /// A float.
    Float,
    /// A boolean.
    Bool,
    /// A string.
    Str,
    /// A type the reducer cannot name: a vector, a matrix, a list, a
    /// struct, or a type that carries a value domain.
    ///
    /// A binding of this class is never replaced. A value domain is part
    /// of the binding's type, and a replacement at the use site would
    /// drop it.
    Other,
}

/// The class of a declared type.
fn class_of_type(ty: &Type) -> Class {
    match ty {
        Type::Float(None) => Class::Float,
        Type::Int(None) => Class::Int,
        Type::Uint(None) => Class::Uint,
        Type::Bool => Class::Bool,
        Type::String => Class::Str,
        _ => Class::Other,
    }
}

/// The class of an expression.
///
/// An expression the reducer cannot classify is a float: that is the
/// emitter's own default for an unannotated local, so treating it as a
/// float keeps the two in step.
fn class_of_expr(expr: &Expr) -> Class {
    match expr {
        Expr::IntLit { .. } => Class::Int,
        Expr::FloatLit { .. } => Class::Float,
        Expr::BoolLit { .. } => Class::Bool,
        Expr::StringLit { .. } => Class::Str,
        Expr::Binary { op, lhs, rhs, .. } => match op {
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
            | BinOp::And | BinOp::Or => Class::Bool,
            _ => match (class_of_expr(lhs), class_of_expr(rhs)) {
                (Class::Int, Class::Int) => Class::Int,
                (Class::Uint, Class::Uint) => Class::Uint,
                _ => Class::Float,
            },
        },
        Expr::Unary { op, expr, .. } => match op {
            UnaryOp::Neg => class_of_expr(expr),
            UnaryOp::Not => Class::Bool,
        },
        _ => Class::Float,
    }
}

// ============================================================================
// Report
// ============================================================================

/// What a pass did, for the diagnostics report.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Bindings that were made read-only.
    pub readonly: Vec<String>,
    /// Bindings that were inlined at their use site.
    pub inlined: Vec<String>,
    /// Bindings that were replaced by a compile-time constant.
    pub folded: Vec<String>,
    /// Expressions that were folded at compile time.
    pub const_folded: usize,
}

impl Report {
    /// Fold another report into this one.
    pub fn merge(&mut self, other: Report) {
        self.readonly.extend(other.readonly);
        self.inlined.extend(other.inlined);
        self.folded.extend(other.folded);
        self.const_folded += other.const_folded;
    }
}

// ============================================================================
// Switches
// ============================================================================

/// The switches that decide which reductions run.
#[derive(Debug, Clone, Copy, Default)]
pub struct Switches {
    /// A binding that is never reassigned stops being mutable.
    pub readonly_locals: bool,
    /// A binding used exactly once is inlined at its use site.
    pub inline_single_use: bool,
    /// A binding whose value is known at compile time becomes that value.
    pub fold_constants: bool,
    /// Constants are folded at compile time inside expressions.
    pub const_fold_arith: bool,
    /// A function nothing references is not emitted.
    ///
    /// This one is off by default for a reason a host can feel: a
    /// library is loaded by symbol name, so a function that disappears
    /// from the module is a function the host can no longer look up.
    pub dead_code_elim: bool,
}

// ============================================================================
// The reduction
// ============================================================================

/// The outcome of reducing one body.
#[derive(Debug, Clone, Default)]
pub struct Reduction {
    /// The rewritten body.
    pub body: Vec<Stmt>,
    /// The locals that are never reassigned.
    pub readonly: HashSet<String>,
    /// What the passes did.
    pub report: Report,
}

/// Reduce one function body.
///
/// The body is a list of statements in source order. The passes run in
/// order: first the constant expressions are folded (so a binding's
/// initialiser is already a constant when the binding passes look at
/// it), then the bindings are folded into their use sites.
pub fn reduce_body(
    stmts: &[Stmt],
    switches: Switches,
    max_depth: usize,
    label: &str,
) -> Reduction {
    let mut report = Report::default();

    // The constant-expression fold runs first: it makes the initialisers
    // of the bindings constant, which is what the binding passes need.
    let stmts = if switches.const_fold_arith {
        let (folded, count) = fold_exprs_in_stmts(stmts, max_depth);
        report.const_folded = count;
        folded
    } else {
        stmts.to_vec()
    };

    let mut analysis = analyse(&stmts, max_depth);
    decide(&mut analysis.bindings, switches, &mut report, label);

    // The read-only set is what the emitter needs; without the switch
    // the emitter keeps writing `let mut` for every local, exactly as
    // it did before the optimiser existed.
    let readonly = if switches.readonly_locals {
        readonly_names(&analysis.bindings)
    } else {
        HashSet::new()
    };
    if switches.readonly_locals {
        for b in &analysis.bindings {
            if !b.assigned {
                report.readonly.push(format!("{label}::{}", b.name));
            }
        }
    }

    let body = rewrite_body(&stmts, &analysis.scopes, &analysis.bindings);
    // The replacement put literals where the bindings were, and a
    // literal can take part in a constant expression. Folding once more
    // makes the result a fixed point: without it, `a + b` with two
    // constant bindings would only fold on the second run.
    let body = if switches.const_fold_arith {
        let (folded, count) = fold_exprs_in_stmts(&body, max_depth);
        report.const_folded += count;
        folded
    } else {
        body
    };
    Reduction { body, readonly, report }
}

// ============================================================================
// Analysis
// ============================================================================

/// A binding the optimiser knows about.
#[derive(Debug, Clone)]
struct Binding {
    /// The name the binding introduces.
    name: String,
    /// The initialiser.
    value: Expr,
    /// The declared type, when the `let` has one.
    ty: Option<Type>,
    /// The span of the `let` statement.
    span: Span,
    /// The compile-time value of the initialiser, when it folds to one.
    constant: Option<ConstValue>,
    /// How many times the name is read.
    reads: usize,
    /// Whether the name is ever assigned to.
    assigned: bool,
    /// Whether the binding may be replaced at all. A destructuring
    /// `let` and a loop variable may not.
    replaceable: bool,
    /// The expression that replaces the binding at its use sites.
    substitution: Option<Expr>,
}

/// The scope structure of a body.
///
/// The tree mirrors the statement list: every `if` contributes two
/// children (the branches), every `for` and `while` one (the body).
#[derive(Debug, Default)]
struct Scopes {
    /// The bindings that are in scope when the scope opens, before any
    /// of its own `let` statements: the loop variable of a `for`.
    prefix: Vec<usize>,
    /// The bindings each `let` in this scope introduces, in declaration
    /// order. A destructuring `let` contributes more than one.
    lets: Vec<Vec<usize>>,
    /// The nested scopes, in the order their blocks appear.
    children: Vec<Scopes>,
}

/// The analysis of a body.
struct Analysis {
    bindings: Vec<Binding>,
    scopes: Scopes,
}

/// Analyse a body: where every name is bound, how often it is read, and
/// whether it is ever assigned.
fn analyse(stmts: &[Stmt], max_depth: usize) -> Analysis {
    let mut bindings: Vec<Binding> = Vec::new();
    let mut scopes = Scopes::default();
    let mut stack: Vec<Vec<usize>> = vec![Vec::new()];
    walk(stmts, &mut bindings, &mut scopes, &mut stack, max_depth);
    Analysis { bindings, scopes }
}

fn walk(
    stmts: &[Stmt],
    bindings: &mut Vec<Binding>,
    scopes: &mut Scopes,
    stack: &mut Vec<Vec<usize>>,
    max_depth: usize,
) {
    for stmt in stmts {
        match stmt {
            Stmt::Let { names, ty, value, span } => {
                count_reads(value, stack, bindings);
                let group: Vec<usize> = names
                    .iter()
                    .map(|name| {
                        let idx = bindings.len();
                        bindings.push(Binding {
                            name: name.clone(),
                            value: value.clone(),
                            ty: ty.clone(),
                            span: span.clone(),
                            constant: const_eval::eval(value, 0, max_depth),
                            reads: 0,
                            assigned: false,
                            replaceable: names.len() == 1,
                            substitution: None,
                        });
                        idx
                    })
                    .collect();
                stack.last_mut().expect("a scope").extend(group.iter().copied());
                scopes.lets.push(group);
            }
            Stmt::Assign { name, value, .. } => {
                count_reads(value, stack, bindings);
                if let Some(idx) = resolve(stack, bindings, name) {
                    bindings[idx].assigned = true;
                }
            }
            Stmt::Return { value: Some(v), .. } => count_reads(v, stack, bindings),
            Stmt::Return { value: None, .. } => {}
            Stmt::Expr(e, _) => count_reads(e, stack, bindings),
            Stmt::If { cond, then_branch, else_branch, .. } => {
                count_reads(cond, stack, bindings);
                for branch in [then_branch, else_branch] {
                    stack.push(Vec::new());
                    let mut child = Scopes::default();
                    walk(&branch.stmts, bindings, &mut child, stack, max_depth);
                    stack.pop();
                    scopes.children.push(child);
                }
            }
            Stmt::For { var, body, .. } => {
                stack.push(Vec::new());
                let idx = bindings.len();
                bindings.push(Binding {
                    name: var.clone(),
                    value: Expr::Ident { name: var.clone(), span: Span::dummy() },
                    ty: None,
                    span: Span::dummy(),
                    constant: None,
                    reads: 0,
                    // The loop assigns the variable on every iteration.
                    assigned: true,
                    replaceable: false,
                    substitution: None,
                });
                stack.last_mut().expect("a scope").push(idx);
                let mut child = Scopes::default();
                child.prefix.push(idx);
                walk(&body.stmts, bindings, &mut child, stack, max_depth);
                stack.pop();
                scopes.children.push(child);
            }
            Stmt::While { cond, body, .. } => {
                count_reads(cond, stack, bindings);
                stack.push(Vec::new());
                let mut child = Scopes::default();
                walk(&body.stmts, bindings, &mut child, stack, max_depth);
                stack.pop();
                scopes.children.push(child);
            }
            _ => {}
        }
    }
}

/// The binding a name refers to at this point: the innermost scope that
/// binds it, and within that scope the most recent declaration.
fn resolve(stack: &[Vec<usize>], bindings: &[Binding], name: &str) -> Option<usize> {
    for scope in stack.iter().rev() {
        for idx in scope.iter().rev() {
            if bindings[*idx].name == name {
                return Some(*idx);
            }
        }
    }
    None
}

/// Count how often each name is read in an expression.
fn count_reads(expr: &Expr, stack: &[Vec<usize>], bindings: &mut [Binding]) {
    match expr {
        Expr::Ident { name, .. } => {
            if let Some(idx) = resolve(stack, bindings, name) {
                bindings[idx].reads += 1;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            count_reads(lhs, stack, bindings);
            count_reads(rhs, stack, bindings);
        }
        Expr::Unary { expr, .. } => count_reads(expr, stack, bindings),
        Expr::Call { callee, args, .. } => {
            count_reads(callee, stack, bindings);
            for a in args {
                count_reads(&a.value, stack, bindings);
            }
        }
        Expr::FieldAccess { target, .. } => count_reads(target, stack, bindings),
        Expr::Index { target, index, .. } => {
            count_reads(target, stack, bindings);
            count_reads(index, stack, bindings);
        }
        Expr::Tuple { elements, .. } | Expr::ListLit { elements, .. } => {
            for e in elements {
                count_reads(e, stack, bindings);
            }
        }
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                count_reads(v, stack, bindings);
            }
        }
        Expr::IfExpr { cond, then_branch, else_branch, .. } => {
            count_reads(cond, stack, bindings);
            count_reads(then_branch, stack, bindings);
            count_reads(else_branch, stack, bindings);
        }
        Expr::AsType { value, .. } | Expr::NarrowCast { value, .. } | Expr::Conv { value, .. } => {
            count_reads(value, stack, bindings)
        }
        Expr::SpasioOutput { value, .. } => count_reads(value, stack, bindings),
        Expr::Spawn { func, args, .. } | Expr::Parallel { func, args, .. } => {
            count_reads(func, stack, bindings);
            for a in args {
                count_reads(&a.value, stack, bindings);
            }
        }
        Expr::Await { task, .. } => count_reads(task, stack, bindings),
        Expr::Send { message, .. } => count_reads(message, stack, bindings),
        Expr::Broadcast { message, .. } => count_reads(message, stack, bindings),
        Expr::NativeCall { args, .. } => {
            for a in args {
                count_reads(&a.value, stack, bindings);
            }
        }
        _ => {}
    }
}

/// The names that are never reassigned anywhere in the body.
///
/// A name is read-only only when *no* binding of that name is assigned
/// to: the emitter decides `let` versus `let mut` per name, so a name
/// that is assigned in one scope and read-only in another has to stay
/// mutable in both.
fn readonly_names(bindings: &[Binding]) -> HashSet<String> {
    let assigned: HashSet<&str> = bindings
        .iter()
        .filter(|b| b.assigned)
        .map(|b| b.name.as_str())
        .collect();
    let mut out = HashSet::new();
    for b in bindings {
        if !assigned.contains(b.name.as_str()) {
            out.insert(b.name.clone());
        }
    }
    out
}

// ============================================================================
// The decisions
// ============================================================================

/// Decide what replaces each binding.
///
/// A binding is replaced when it is never assigned to and either
///
/// * its initialiser folds to a compile-time constant whose type is the
/// binding's type, or
/// * it is read exactly once and its initialiser has no side effect.
fn decide(bindings: &mut [Binding], switches: Switches, report: &mut Report, label: &str) {
    for b in bindings.iter_mut() {
        if !b.replaceable || b.assigned {
            continue;
        }
        // The class of the binding: its declared type, or the class of
        // its initialiser when it has no declaration.
        let class = match &b.ty {
            Some(t) => class_of_type(t),
            None => class_of_expr(&b.value),
        };
        if class == Class::Other {
            continue;
        }
        if switches.fold_constants {
            if let Some(c) = &b.constant {
                let folded = c.to_expr(b.span.clone());
                // The replacement has to have the binding's type: a
                // use site expects what the binding's type says.
                if class_of_expr(&folded) == class {
                    b.substitution = Some(folded);
                    report.folded.push(format!("{label}::{}", b.name));
                    continue;
                }
            }
        }
        if switches.inline_single_use
            && b.reads == 1
            && !const_eval::has_side_effect(&b.value)
        {
            b.substitution = Some(b.value.clone());
            report.inlined.push(format!("{label}::{}", b.name));
        }
    }
}

// ============================================================================
// The rewrite
// ============================================================================

fn rewrite_body(stmts: &[Stmt], scopes: &Scopes, bindings: &[Binding]) -> Vec<Stmt> {
    let mut stack: Vec<Vec<usize>> = vec![Vec::new()];
    let mut out = Vec::with_capacity(stmts.len());
    rewrite_stmts(stmts, scopes, bindings, &mut stack, &mut out);
    out
}

fn rewrite_stmts(
    stmts: &[Stmt],
    scopes: &Scopes,
    bindings: &[Binding],
    stack: &mut Vec<Vec<usize>>,
    out: &mut Vec<Stmt>,
) {
    let mut let_index = 0usize;
    let mut child_index = 0usize;
    for stmt in stmts {
        match stmt {
            Stmt::Let { names, ty, value, span } => {
                let group = scopes.lets.get(let_index).cloned().unwrap_or_default();
                let_index += 1;
                // The initialiser is evaluated before the binding exists,
                // so it is rewritten against the scope as of *now*.
                let value = rewrite_expr(value, stack, bindings);
                let replaced = names.len() == 1
                    && group.len() == 1
                    && bindings[group[0]].substitution.is_some();
                if !replaced {
                    out.push(Stmt::Let {
                        names: names.clone(),
                        ty: ty.clone(),
                        value,
                        span: span.clone(),
                    });
                }
                // The binding exists from here on, whether or not the
                // statement survived: its use sites still refer to it.
                stack.last_mut().expect("a scope").extend(group.iter().copied());
            }
            Stmt::Assign { name, value, span } => {
                let value = rewrite_expr(value, stack, bindings);
                out.push(Stmt::Assign { name: name.clone(), value, span: span.clone() });
            }
            Stmt::Return { value, span } => {
                let value = value.as_ref().map(|v| rewrite_expr(v, stack, bindings));
                out.push(Stmt::Return { value, span: span.clone() });
            }
            Stmt::Expr(e, span) => {
                out.push(Stmt::Expr(rewrite_expr(e, stack, bindings), span.clone()))
            }
            Stmt::If { cond, then_branch, else_branch, span } => {
                let cond = rewrite_expr(cond, stack, bindings);
                let then_scopes = scopes.children.get(child_index);
                let else_scopes = scopes.children.get(child_index + 1);
                child_index += 2;
                let then_branch = rewrite_block(then_branch, then_scopes, bindings, stack);
                let else_branch = rewrite_block(else_branch, else_scopes, bindings, stack);
                out.push(Stmt::If { cond, then_branch, else_branch, span: span.clone() });
            }
            Stmt::For { var, iter, body, span } => {
                let child = scopes.children.get(child_index);
                child_index += 1;
                let body = rewrite_block(body, child, bindings, stack);
                out.push(Stmt::For {
                    var: var.clone(),
                    iter: iter.clone(),
                    body,
                    span: span.clone(),
                });
            }
            Stmt::While { cond, body, span } => {
                let cond = rewrite_expr(cond, stack, bindings);
                let child = scopes.children.get(child_index);
                child_index += 1;
                let body = rewrite_block(body, child, bindings, stack);
                out.push(Stmt::While { cond, body, span: span.clone() });
            }
            other => out.push(other.clone()),
        }
    }
}

fn rewrite_block(
    block: &Block,
    scopes: Option<&Scopes>,
    bindings: &[Binding],
    stack: &mut Vec<Vec<usize>>,
) -> Block {
    let mut out = Vec::with_capacity(block.stmts.len());
    stack.push(Vec::new());
    match scopes {
        Some(s) => {
            stack
                .last_mut()
                .expect("a scope")
                .extend(s.prefix.iter().copied());
            rewrite_stmts(&block.stmts, s, bindings, stack, &mut out);
        }
        None => out.extend(block.stmts.iter().cloned()),
    }
    stack.pop();
    Block { stmts: out, span: block.span.clone() }
}

/// Replace a name that refers to a replaced binding with the
/// replacement.
fn rewrite_expr(expr: &Expr, stack: &[Vec<usize>], bindings: &[Binding]) -> Expr {
    match expr {
        Expr::Ident { name, .. } => {
            if let Some(idx) = resolve(stack, bindings, name) {
                if let Some(sub) = &bindings[idx].substitution {
                    return sub.clone();
                }
            }
            expr.clone()
        }
        Expr::Binary { op, lhs, rhs, span } => Expr::Binary {
            op: *op,
            lhs: Box::new(rewrite_expr(lhs, stack, bindings)),
            rhs: Box::new(rewrite_expr(rhs, stack, bindings)),
            span: span.clone(),
        },
        Expr::Unary { op, expr: inner, span } => Expr::Unary {
            op: *op,
            expr: Box::new(rewrite_expr(inner, stack, bindings)),
            span: span.clone(),
        },
        Expr::Call { callee, args, span } => Expr::Call {
            callee: Box::new(rewrite_expr(callee, stack, bindings)),
            args: args
                .iter()
                .map(|a| spass_parser::ast::CallArg {
                    value: rewrite_expr(&a.value, stack, bindings),
                    span: a.span.clone(),
                })
                .collect(),
            span: span.clone(),
        },
        Expr::FieldAccess { target, field, span } => Expr::FieldAccess {
            target: Box::new(rewrite_expr(target, stack, bindings)),
            field: field.clone(),
            span: span.clone(),
        },
        Expr::Index { target, index, span } => Expr::Index {
            target: Box::new(rewrite_expr(target, stack, bindings)),
            index: Box::new(rewrite_expr(index, stack, bindings)),
            span: span.clone(),
        },
        Expr::Tuple { elements, span } => Expr::Tuple {
            elements: elements
                .iter()
                .map(|e| rewrite_expr(e, stack, bindings))
                .collect(),
            span: span.clone(),
        },
        Expr::ListLit { elements, span } => Expr::ListLit {
            elements: elements
                .iter()
                .map(|e| rewrite_expr(e, stack, bindings))
                .collect(),
            span: span.clone(),
        },
        Expr::StructLit { name, fields, span } => Expr::StructLit {
            name: name.clone(),
            fields: fields
                .iter()
                .map(|(n, v)| (n.clone(), rewrite_expr(v, stack, bindings)))
                .collect(),
            span: span.clone(),
        },
        Expr::IfExpr { cond, then_branch, else_branch, span } => Expr::IfExpr {
            cond: Box::new(rewrite_expr(cond, stack, bindings)),
            then_branch: Box::new(rewrite_expr(then_branch, stack, bindings)),
            else_branch: Box::new(rewrite_expr(else_branch, stack, bindings)),
            span: span.clone(),
        },
        Expr::AsType { value, target, span } => Expr::AsType {
            value: Box::new(rewrite_expr(value, stack, bindings)),
            target: target.clone(),
            span: span.clone(),
        },
        Expr::NarrowCast { value, target, span } => Expr::NarrowCast {
            value: Box::new(rewrite_expr(value, stack, bindings)),
            target: target.clone(),
            span: span.clone(),
        },
        Expr::Conv { kind, value, span } => Expr::Conv {
            kind: kind.clone(),
            value: Box::new(rewrite_expr(value, stack, bindings)),
            span: span.clone(),
        },
        Expr::SpasioOutput { mode, value, channel, span } => Expr::SpasioOutput {
            mode: mode.clone(),
            value: Box::new(rewrite_expr(value, stack, bindings)),
            channel: channel.clone(),
            span: span.clone(),
        },
        Expr::Spawn { func, args, annotations, span } => Expr::Spawn {
            func: Box::new(rewrite_expr(func, stack, bindings)),
            args: args
                .iter()
                .map(|a| spass_parser::ast::CallArg {
                    value: rewrite_expr(&a.value, stack, bindings),
                    span: a.span.clone(),
                })
                .collect(),
            annotations: annotations.clone(),
            span: span.clone(),
        },
        Expr::Parallel { func, args, annotations, span } => Expr::Parallel {
            func: Box::new(rewrite_expr(func, stack, bindings)),
            args: args
                .iter()
                .map(|a| spass_parser::ast::CallArg {
                    value: rewrite_expr(&a.value, stack, bindings),
                    span: a.span.clone(),
                })
                .collect(),
            annotations: annotations.clone(),
            span: span.clone(),
        },
        Expr::Await { task, span } => Expr::Await {
            task: Box::new(rewrite_expr(task, stack, bindings)),
            span: span.clone(),
        },
        Expr::Send { target, message, span } => Expr::Send {
            target: target.clone(),
            message: Box::new(rewrite_expr(message, stack, bindings)),
            span: span.clone(),
        },
        Expr::Broadcast { message, span } => Expr::Broadcast {
            message: Box::new(rewrite_expr(message, stack, bindings)),
            span: span.clone(),
        },
        Expr::NativeCall { namespace, func, args, span } => Expr::NativeCall {
            namespace: namespace.clone(),
            func: func.clone(),
            args: args
                .iter()
                .map(|a| spass_parser::ast::CallArg {
                    value: rewrite_expr(&a.value, stack, bindings),
                    span: a.span.clone(),
                })
                .collect(),
            span: span.clone(),
        },
        other => other.clone(),
    }
}

// ============================================================================
// Constant folding
// ============================================================================

/// Fold the constant sub-expressions of every statement in a body.
///
/// Returns the rewritten statements and how many expressions were folded.
fn fold_exprs_in_stmts(stmts: &[Stmt], max_depth: usize) -> (Vec<Stmt>, usize) {
    let mut count = 0usize;
    let out = stmts
        .iter()
        .map(|s| {
            let (stmt, n) = fold_exprs_in_stmt(s, max_depth);
            count += n;
            stmt
        })
        .collect();
    (out, count)
}

fn fold_exprs_in_stmt(stmt: &Stmt, max_depth: usize) -> (Stmt, usize) {
    match stmt {
        Stmt::Let { names, ty, value, span } => {
            let (value, n) = fold_expr(value, max_depth);
            (Stmt::Let { names: names.clone(), ty: ty.clone(), value, span: span.clone() }, n)
        }
        Stmt::Assign { name, value, span } => {
            let (value, n) = fold_expr(value, max_depth);
            (Stmt::Assign { name: name.clone(), value, span: span.clone() }, n)
        }
        Stmt::Return { value, span } => {
            let (value, n) = match value {
                Some(v) => {
                    let (v, n) = fold_expr(v, max_depth);
                    (Some(v), n)
                }
                None => (None, 0),
            };
            (Stmt::Return { value, span: span.clone() }, n)
        }
        Stmt::Expr(e, span) => {
            let (e, n) = fold_expr(e, max_depth);
            (Stmt::Expr(e, span.clone()), n)
        }
        Stmt::If { cond, then_branch, else_branch, span } => {
            let (cond, n1) = fold_expr(cond, max_depth);
            let (then_branch, n2) = fold_exprs_in_block(then_branch, max_depth);
            let (else_branch, n3) = fold_exprs_in_block(else_branch, max_depth);
            (
                Stmt::If { cond, then_branch, else_branch, span: span.clone() },
                n1 + n2 + n3,
            )
        }
        Stmt::For { var, iter, body, span } => {
            let (body, n) = fold_exprs_in_block(body, max_depth);
            (Stmt::For { var: var.clone(), iter: iter.clone(), body, span: span.clone() }, n)
        }
        Stmt::While { cond, body, span } => {
            let (cond, n1) = fold_expr(cond, max_depth);
            let (body, n2) = fold_exprs_in_block(body, max_depth);
            (Stmt::While { cond, body, span: span.clone() }, n1 + n2)
        }
        other => (other.clone(), 0),
    }
}

fn fold_exprs_in_block(block: &Block, max_depth: usize) -> (Block, usize) {
    let (stmts, n) = fold_exprs_in_stmts(&block.stmts, max_depth);
    (Block { stmts, span: block.span.clone() }, n)
}

/// Fold the constant sub-expressions of one expression.
fn fold_expr(expr: &Expr, max_depth: usize) -> (Expr, usize) {
    // Rebuild the expression with folded children first, then try to fold
    // the whole thing.
    let (rebuilt, mut count) = match expr {
        Expr::Binary { op, lhs, rhs, span } => {
            let (lhs, n1) = fold_expr(lhs, max_depth);
            let (rhs, n2) = fold_expr(rhs, max_depth);
            (
                Expr::Binary {
                    op: *op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    span: span.clone(),
                },
                n1 + n2,
            )
        }
        Expr::Unary { op, expr: inner, span } => {
            let (inner, n) = fold_expr(inner, max_depth);
            (
                Expr::Unary { op: *op, expr: Box::new(inner), span: span.clone() },
                n,
            )
        }
        Expr::Call { callee, args, span } => {
            let (callee, n1) = fold_expr(callee, max_depth);
            let mut n2 = 0usize;
            let args: Vec<_> = args
                .iter()
                .map(|a| {
                    let (value, n) = fold_expr(&a.value, max_depth);
                    n2 += n;
                    spass_parser::ast::CallArg { value, span: a.span.clone() }
                })
                .collect();
            (
                Expr::Call { callee: Box::new(callee), args, span: span.clone() },
                n1 + n2,
            )
        }
        Expr::Index { target, index, span } => {
            let (target, n1) = fold_expr(target, max_depth);
            let (index, n2) = fold_expr(index, max_depth);
            (
                Expr::Index {
                    target: Box::new(target),
                    index: Box::new(index),
                    span: span.clone(),
                },
                n1 + n2,
            )
        }
        Expr::FieldAccess { target, field, span } => {
            let (target, n) = fold_expr(target, max_depth);
            (
                Expr::FieldAccess { target: Box::new(target), field: field.clone(), span: span.clone() },
                n,
            )
        }
        Expr::Tuple { elements, span } => {
            let mut n = 0usize;
            let elements: Vec<_> = elements
                .iter()
                .map(|e| {
                    let (e, c) = fold_expr(e, max_depth);
                    n += c;
                    e
                })
                .collect();
            (Expr::Tuple { elements, span: span.clone() }, n)
        }
        Expr::ListLit { elements, span } => {
            let mut n = 0usize;
            let elements: Vec<_> = elements
                .iter()
                .map(|e| {
                    let (e, c) = fold_expr(e, max_depth);
                    n += c;
                    e
                })
                .collect();
            (Expr::ListLit { elements, span: span.clone() }, n)
        }
        Expr::StructLit { name, fields, span } => {
            let mut n = 0usize;
            let fields: Vec<_> = fields
                .iter()
                .map(|(k, v)| {
                    let (v, c) = fold_expr(v, max_depth);
                    n += c;
                    (k.clone(), v)
                })
                .collect();
            (Expr::StructLit { name: name.clone(), fields, span: span.clone() }, n)
        }
        Expr::IfExpr { cond, then_branch, else_branch, span } => {
            let (cond, n1) = fold_expr(cond, max_depth);
            let (then_branch, n2) = fold_expr(then_branch, max_depth);
            let (else_branch, n3) = fold_expr(else_branch, max_depth);
            (
                Expr::IfExpr {
                    cond: Box::new(cond),
                    then_branch: Box::new(then_branch),
                    else_branch: Box::new(else_branch),
                    span: span.clone(),
                },
                n1 + n2 + n3,
            )
        }
        Expr::AsType { value, target, span } => {
            let (value, n) = fold_expr(value, max_depth);
            (
                Expr::AsType { value: Box::new(value), target: target.clone(), span: span.clone() },
                n,
            )
        }
        Expr::NarrowCast { value, target, span } => {
            let (value, n) = fold_expr(value, max_depth);
            (
                Expr::NarrowCast { value: Box::new(value), target: target.clone(), span: span.clone() },
                n,
            )
        }
        Expr::Conv { kind, value, span } => {
            let (value, n) = fold_expr(value, max_depth);
            (
                Expr::Conv { kind: kind.clone(), value: Box::new(value), span: span.clone() },
                n,
            )
        }
        other => (other.clone(), 0),
    };

    // Now try to fold the rebuilt expression itself. The result must
    // differ from the original, or the fold would loop forever.
    if let Some(v) = const_eval::eval(&rebuilt, 0, max_depth) {
        let folded = v.to_expr(expr_span(expr));
        if folded != rebuilt {
            count += 1;
            return (folded, count);
        }
    }
    (rebuilt, count)
}

fn expr_span(expr: &Expr) -> Span {
    match expr {
        Expr::IntLit { span, .. }
        | Expr::FloatLit { span, .. }
        | Expr::BoolLit { span, .. }
        | Expr::StringLit { span, .. }
        | Expr::Ident { span, .. }
        | Expr::Binary { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Call { span, .. }
        | Expr::FieldAccess { span, .. }
        | Expr::Index { span, .. }
        | Expr::Tuple { span, .. }
        | Expr::ListLit { span, .. }
        | Expr::StructLit { span, .. }
        | Expr::IfExpr { span, .. }
        | Expr::Spawn { span, .. }
        | Expr::Parallel { span, .. }
        | Expr::Await { span, .. }
        | Expr::Random { span, .. }
        | Expr::AsType { span, .. }
        | Expr::NarrowCast { span, .. }
        | Expr::Conv { span, .. }
        | Expr::SpasioInput { span, .. }
        | Expr::SpasioOutput { span, .. }
        | Expr::NativeCall { span, .. }
        | Expr::Send { span, .. }
        | Expr::Broadcast { span, .. }
        | Expr::Receive { span, .. }
        | Expr::IfPresent { span, .. } => span.clone(),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use spass_parser::ast::BinOp;

    fn int(v: i64) -> Expr {
        Expr::IntLit { value: v, span: Span::dummy() }
    }

    fn ident(name: &str) -> Expr {
        Expr::Ident { name: name.to_string(), span: Span::dummy() }
    }

    fn bin(op: BinOp, l: Expr, r: Expr) -> Expr {
        Expr::Binary { op, lhs: Box::new(l), rhs: Box::new(r), span: Span::dummy() }
    }

    fn let_stmt(name: &str, value: Expr) -> Stmt {
        Stmt::Let { names: vec![name.to_string()], ty: None, value, span: Span::dummy() }
    }

    fn assign(name: &str, value: Expr) -> Stmt {
        Stmt::Assign { name: name.to_string(), value, span: Span::dummy() }
    }

    fn return_stmt(value: Expr) -> Stmt {
        Stmt::Return { value: Some(value), span: Span::dummy() }
    }

    fn all_switches() -> Switches {
        Switches {
            readonly_locals: true,
            inline_single_use: true,
            fold_constants: true,
            const_fold_arith: true,
            dead_code_elim: false,
        }
    }

    /// Only the inlining pass. The folding pass would otherwise take
    /// the binding first: a constant that is read once is folded into
    /// its use site rather than inlined there.
    fn inline_only() -> Switches {
        Switches {
            readonly_locals: true,
            inline_single_use: true,
            fold_constants: false,
            const_fold_arith: false,
            dead_code_elim: false,
        }
    }

    /// A binding that is never reassigned is reported as read-only.
    #[test]
    fn a_binding_that_is_never_reassigned_is_readonly() {
        let stmts = vec![let_stmt("a", int(1)), return_stmt(ident("a"))];
        let r = reduce_body(&stmts, all_switches(), 8, "f");
        assert!(r.report.readonly.iter().any(|x| x == "f::a"));
        assert!(r.readonly.contains("a"));
    }

    /// A binding that *is* reassigned is not a constant and must not be
    /// reported as read-only.
    #[test]
    fn a_reassigned_binding_is_not_readonly() {
        let stmts = vec![
            let_stmt("a", int(1)),
            assign("a", int(2)),
            return_stmt(ident("a")),
        ];
        let r = reduce_body(&stmts, all_switches(), 8, "f");
        assert!(!r.report.readonly.iter().any(|x| x == "f::a"));
        assert!(!r.readonly.contains("a"));
    }

    /// A binding used exactly once moves to its use site.
    #[test]
    fn a_single_use_binding_is_inlined() {
        let stmts = vec![let_stmt("a", int(7)), return_stmt(bin(BinOp::Add, ident("a"), int(1)))];
        let r = reduce_body(&stmts, inline_only(), 8, "f");
        assert!(r.report.inlined.iter().any(|x| x == "f::a"));
        // The binding is gone and the literal sits in the return.
        assert!(!r
            .body
            .iter()
            .any(|s| matches!(s, Stmt::Let { names, .. } if names == &["a".to_string()])));
        match &r.body[0] {
            Stmt::Return { value: Some(v), .. } => {
                assert!(matches!(v, Expr::Binary { lhs, .. } if **lhs == int(7)));
            }
            other => panic!("expected a return, got {other:?}"),
        }
    }

    /// A binding used twice cannot be inlined.
    #[test]
    fn a_multi_use_binding_is_not_inlined() {
        let stmts = vec![
            let_stmt("a", int(7)),
            return_stmt(bin(BinOp::Add, ident("a"), ident("a"))),
        ];
        let r = reduce_body(&stmts, inline_only(), 8, "f");
        assert!(r.report.inlined.is_empty());
        assert!(r
            .body
            .iter()
            .any(|s| matches!(s, Stmt::Let { names, .. } if names == &["a".to_string()])));
    }

    /// A constant that is read once is folded rather than inlined: the
    /// fold produces the value itself, which is what the use site gets
    /// either way.
    #[test]
    fn a_constant_read_once_is_folded() {
        let stmts = vec![let_stmt("a", int(7)), return_stmt(bin(BinOp::Add, ident("a"), int(1)))];
        let r = reduce_body(&stmts, all_switches(), 8, "f");
        assert!(r.report.folded.iter().any(|x| x == "f::a"));
        assert!(r.report.inlined.is_empty());
        // The constant stands at the use site, and the constant
        // expression around it folds too.
        match &r.body[0] {
            Stmt::Return { value: Some(v), .. } => assert_eq!(*v, int(8)),
            other => panic!("expected a return, got {other:?}"),
        }
    }

    /// A binding whose initialiser has a side effect is not inlined:
    /// inlining would change how many times the call runs.
    #[test]
    fn a_binding_with_a_side_effect_is_not_inlined() {
        let call = Expr::Call {
            callee: Box::new(ident("f")),
            args: vec![],
            span: Span::dummy(),
        };
        let stmts = vec![let_stmt("a", call), return_stmt(bin(BinOp::Add, ident("a"), int(1)))];
        let r = reduce_body(&stmts, all_switches(), 8, "f");
        assert!(r.report.inlined.is_empty());
    }

    /// A constant binding is folded: the use site takes the value.
    #[test]
    fn a_constant_binding_is_folded() {
        let stmts = vec![let_stmt("a", bin(BinOp::Add, int(2), int(3))), return_stmt(ident("a"))];
        let r = reduce_body(&stmts, all_switches(), 8, "f");
        assert!(r.report.folded.iter().any(|x| x == "f::a"));
        // The binding disappears and the value stands at the use site.
        assert_eq!(r.body.len(), 1);
        match &r.body[0] {
            Stmt::Return { value: Some(v), .. } => assert_eq!(*v, int(5)),
            other => panic!("expected a return, got {other:?}"),
        }
    }

    /// A reassigned binding is not a constant even though its initialiser
    /// folds.
    #[test]
    fn a_reassigned_binding_is_not_folded() {
        let stmts = vec![
            let_stmt("a", bin(BinOp::Add, int(2), int(3))),
            assign("a", int(9)),
            return_stmt(ident("a")),
        ];
        let r = reduce_body(&stmts, all_switches(), 8, "f");
        assert!(r.report.folded.is_empty());
        assert!(r
            .body
            .iter()
            .any(|s| matches!(s, Stmt::Let { names, .. } if names == &["a".to_string()])));
    }

    /// With every switch off the body is unchanged.
    #[test]
    fn nothing_changes_when_every_switch_is_off() {
        let stmts = vec![
            let_stmt("a", bin(BinOp::Add, int(2), int(3))),
            return_stmt(ident("a")),
        ];
        let r = reduce_body(&stmts, Switches::default(), 8, "f");
        assert_eq!(r.body, stmts);
        assert!(r.report.readonly.is_empty());
        assert!(r.report.inlined.is_empty());
        assert!(r.report.folded.is_empty());
        assert_eq!(r.report.const_folded, 0);
        assert!(r.readonly.is_empty());
    }

    /// The reduction is idempotent: running it twice changes nothing the
    /// second time.
    #[test]
    fn the_reduction_is_idempotent() {
        let stmts = vec![
            let_stmt("a", bin(BinOp::Add, int(2), int(3))),
            let_stmt("b", bin(BinOp::Mul, int(4), int(5))),
            return_stmt(bin(BinOp::Add, ident("a"), ident("b"))),
        ];
        let once = reduce_body(&stmts, all_switches(), 8, "f").body;
        let twice = reduce_body(&once, all_switches(), 8, "f").body;
        assert_eq!(once, twice);
    }

    /// The reduction is deterministic: the same input gives the same
    /// output every time.
    #[test]
    fn the_reduction_is_deterministic() {
        let stmts = vec![
            let_stmt("a", bin(BinOp::Add, int(2), int(3))),
            let_stmt("b", bin(BinOp::Mul, int(4), int(5))),
            let_stmt("c", bin(BinOp::Sub, int(9), int(1))),
            return_stmt(bin(BinOp::Add, ident("a"), bin(BinOp::Add, ident("b"), ident("c")))),
        ];
        let first = reduce_body(&stmts, all_switches(), 8, "f").body;
        for _ in 0..5 {
            let again = reduce_body(&stmts, all_switches(), 8, "f").body;
            assert_eq!(first, again);
        }
    }

    /// A constant expression inside a statement is folded at compile time.
    #[test]
    fn constant_expressions_are_folded() {
        let stmts = vec![return_stmt(bin(BinOp::Add, int(2), bin(BinOp::Mul, int(3), int(4))))];
        let r = reduce_body(&stmts, all_switches(), 8, "f");
        assert!(r.report.const_folded > 0);
        match &r.body[0] {
            Stmt::Return { value: Some(v), .. } => assert_eq!(*v, int(14)),
            other => panic!("expected a return, got {other:?}"),
        }
    }

    /// A binding that a nested scope shadows is not replaced: the inner
    /// binding is a different one.
    #[test]
    fn a_shadowed_binding_is_not_replaced() {
        let inner = Stmt::If {
            cond: ident("c"),
            then_branch: Block {
                stmts: vec![let_stmt("a", int(9)), return_stmt(ident("a"))],
                span: Span::dummy(),
            },
            else_branch: Block { stmts: vec![], span: Span::dummy() },
            span: Span::dummy(),
        };
        let stmts = vec![let_stmt("a", int(1)), inner, return_stmt(ident("a"))];
        let r = reduce_body(&stmts, all_switches(), 8, "f");
        // The outer binding is read once, so it is inlined at its own
        // use site; the inner binding keeps its own value.
        let inner_return = r
            .body
            .iter()
            .find_map(|s| match s {
                Stmt::If { then_branch, .. } => then_branch.stmts.last(),
                _ => None,
            })
            .expect("the if statement");
        match inner_return {
            Stmt::Return { value: Some(v), .. } => assert_eq!(*v, int(9)),
            other => panic!("expected a return, got {other:?}"),
        }
    }

    /// A binding that is assigned inside a nested branch is not a
    /// constant, even though the assignment is not at the top level.
    #[test]
    fn a_binding_assigned_in_a_branch_is_not_folded() {
        let inner = Stmt::If {
            cond: ident("c"),
            then_branch: Block {
                stmts: vec![assign("a", int(9))],
                span: Span::dummy(),
            },
            else_branch: Block { stmts: vec![], span: Span::dummy() },
            span: Span::dummy(),
        };
        let stmts = vec![let_stmt("a", bin(BinOp::Add, int(2), int(3))), inner, return_stmt(ident("a"))];
        let r = reduce_body(&stmts, all_switches(), 8, "f");
        assert!(r.report.folded.is_empty());
    }
}
