//! Detects which runtime sections a generated module needs.

use crate::{no_readonly, vec_mat_builtin, Ctx};
use spass_parser::ast::{AssertKind, CallArg, DispatchBindSource, Expr, ForIter, Stmt, Type};
use spass_semantic::TypedModule;

/// The runtime sections a generated module needs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeSections {
    /// vector / matrix types, arithmetic and built-ins.
    pub vec_mat: bool,
    /// the spasio client (connection pool + frame protocol).
    pub spasio: bool,
    /// the async task registry (`spawn` / `await`).
    pub tasks: bool,
    /// sequence queues and message passing.
    pub sequences: bool,
    /// The playground runtime (`s2d:` / `s3d:` / `spio:` namespaces).
    pub playground: bool,
}

impl RuntimeSections {
    /// Determine the sections a module needs from its signatures and
    /// from the expressions its bodies contain.
    ///
    /// The vec/mat section is decided by the same helper the emitters
    /// use to dispatch `dot` / `length` /... to a dimension-specific
    /// runtime symbol, so a program that calls `length` on a list does
    /// not pull in the vector runtime.
    pub fn detect(typed: &TypedModule) -> Self {
        let mut s = Self::default();
        s.sequences = !typed.sequences.is_empty();
        s.spasio = !typed.channels.is_empty();

        for func in &typed.functions {
            for p in &func.params {
                s.note_type(&p.ty);
            }
            if let Some(t) = &func.return_type {
                s.note_type(t);
            }
            if let Some(mr) = &func.multi_returns {
                for t in mr {
                    s.note_type(t);
                }
            }
            let cx = Ctx::new(func, typed, None, &func.original.body.stmts, &no_readonly());
            s.scan_stmts(&func.original.body.stmts, Some(&cx));
        }
        for st in &typed.structs {
            for (_, t) in &st.fields {
                s.note_type(t);
            }
        }
        for seq in &typed.sequences {
            s.scan_stmts(&seq.body.stmts, None);
        }
        s
    }

    /// A vec or mat anywhere in a type means the vec/mat runtime types
    /// have to exist in the generated module.
    fn note_type(&mut self, ty: &Type) {
        match ty {
            Type::Vec2(_, _) | Type::Vec3(_, _, _) | Type::Vec4(_, _, _, _) | Type::Mat3 | Type::Mat4 => {
                self.vec_mat = true;
            }
            Type::List(inner) => self.note_type(inner),
            _ => {}
        }
    }

    fn scan_stmts(&mut self, stmts: &[Stmt], cx: Option<&Ctx<'_>>) {
        for st in stmts {
            self.scan_stmt(st, cx);
        }
    }

    fn scan_stmt(&mut self, st: &Stmt, cx: Option<&Ctx<'_>>) {
        match st {
            Stmt::Let { value, .. } => self.scan_expr(value, cx),
            Stmt::Assign { value, .. } => self.scan_expr(value, cx),
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.scan_expr(v, cx);
                }
            }
            Stmt::Expr(e, _) => self.scan_expr(e, cx),
            Stmt::If { cond, then_branch, else_branch, .. } => {
                self.scan_expr(cond, cx);
                self.scan_stmts(&then_branch.stmts, cx);
                self.scan_stmts(&else_branch.stmts, cx);
            }
            Stmt::For { iter, body, .. } => {
                match iter {
                    ForIter::Range { start, end, step } => {
                        self.scan_expr(start, cx);
                        self.scan_expr(end, cx);
                        if let Some(s) = step {
                            self.scan_expr(s, cx);
                        }
                    }
                    ForIter::List(e) => self.scan_expr(e, cx),
                }
                self.scan_stmts(&body.stmts, cx);
            }
            Stmt::While { cond, body, .. } => {
                self.scan_expr(cond, cx);
                self.scan_stmts(&body.stmts, cx);
            }
            Stmt::Dispatch { decl, apply, .. } => {
                for bind in &decl.bind {
                    if let DispatchBindSource::Literal(e) = &bind.source {
                        self.scan_expr(e, cx);
                    }
                }
                self.scan_expr(&apply.call, cx);
            }
            Stmt::Assert { kind, .. } => {
                if let AssertKind::Approx { target, tolerance, .. } = kind {
                    self.scan_expr(target, cx);
                    self.scan_expr(tolerance, cx);
                }
            }
            Stmt::IfPresent { present_branch, absent_branch, .. } => {
                self.scan_stmts(&present_branch.stmts, cx);
                if let Some(b) = absent_branch {
                    self.scan_stmts(&b.stmts, cx);
                }
            }
            Stmt::Ownclude(f) => self.scan_stmts(&f.body.stmts, cx),
            _ => {}
        }
    }

    fn scan_expr(&mut self, e: &Expr, cx: Option<&Ctx<'_>>) {
        match e {
            Expr::SpasioInput { .. } => self.spasio = true,
            Expr::SpasioOutput { value, .. } => {
                self.spasio = true;
                self.scan_expr(value, cx);
            }
            Expr::Spawn { func, args, .. } | Expr::Parallel { func, args, .. } => {
                self.tasks = true;
                self.scan_expr(func, cx);
                for a in args {
                    self.scan_expr(&a.value, cx);
                }
            }
            Expr::Await { task, .. } => {
                self.tasks = true;
                self.scan_expr(task, cx);
            }
            Expr::Random { candidates, args, .. } => {
                for c in candidates {
                    self.scan_expr(c, cx);
                }
                if let Some(args) = args {
                    for a in args {
                        self.scan_expr(&a.value, cx);
                    }
                }
            }
            Expr::NativeCall { args, .. } => {
                self.playground = true;
                for a in args {
                    self.scan_expr(&a.value, cx);
                }
            }
            Expr::Send { message, .. } | Expr::Broadcast { message, .. } => {
                self.sequences = true;
                self.scan_expr(message, cx);
            }
            Expr::Receive { pattern, .. } => {
                self.sequences = true;
                if let Some(p) = pattern {
                    for (_, _, v) in &p.conditions {
                        self.scan_expr(v, cx);
                    }
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.scan_expr(lhs, cx);
                self.scan_expr(rhs, cx);
            }
            Expr::Unary { expr, .. } => self.scan_expr(expr, cx),
            Expr::Call { callee, args, .. } => {
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    self.note_vec_mat_call(name, args, cx);
                }
                self.scan_expr(callee, cx);
                for a in args {
                    self.scan_expr(&a.value, cx);
                }
            }
            Expr::FieldAccess { target, .. } => self.scan_expr(target, cx),
            Expr::Index { target, index, .. } => {
                self.scan_expr(target, cx);
                self.scan_expr(index, cx);
            }
            Expr::Tuple { elements, .. } | Expr::ListLit { elements, .. } => {
                for el in elements {
                    self.scan_expr(el, cx);
                }
            }
            Expr::StructLit { fields, .. } => {
                for (_, v) in fields {
                    self.scan_expr(v, cx);
                }
            }
            Expr::IfExpr { cond, then_branch, else_branch, .. } => {
                self.scan_expr(cond, cx);
                self.scan_expr(then_branch, cx);
                self.scan_expr(else_branch, cx);
            }
            Expr::AsType { value, .. } | Expr::NarrowCast { value, .. } => self.scan_expr(value, cx),
            Expr::Conv { value, .. } => self.scan_expr(value, cx),
            Expr::IfPresent { then_branch, absent_branch, .. } => {
                self.scan_stmts(&then_branch.stmts, cx);
                if let Some(b) = absent_branch {
                    self.scan_stmts(&b.stmts, cx);
                }
            }
            _ => {}
        }
    }

    /// A vec/mat built-in only needs the vector runtime when it
    /// actually dispatches to a dimension-specific runtime symbol.
    fn note_vec_mat_call(&mut self, name: &str, args: &[CallArg], cx: Option<&Ctx<'_>>) {
        let is_candidate = matches!(
            name,
            "mk_vec2" | "mk_vec3" | "mk_vec4" | "dot" | "length" | "normalize" | "hadamard" | "cross" | "transpose"
        );
        if !is_candidate {
            return;
        }
        // The constructor dispatch is arity-based and needs no type
        // information; the dimension-specific ones need the first
        // argument's type, which only a function scope can provide.
        let resolves = match cx {
            Some(cx) => vec_mat_builtin(name, args, cx).is_some(),
            None => matches!(name, "mk_vec2" | "mk_vec3" | "mk_vec4"),
        };
        if resolves {
            self.vec_mat = true;
        }
    }
}
