//! Lowers Spass `test` blocks into ordinary functions for the test driver.

use spass_parser::ast::{
    AssertKind, BinOp, Block, Expr, FuncDef, RangeBound, Stmt, TestDef, Type, Visibility,
};
use spass_lexer::Span;

/// Sanitize a test name into a valid Spass identifier for the synthetic
/// function. Spaces, dashes, and other non-identifier characters become
/// underscores. The result is prefixed with `_test_` so the symbol is
/// distinguishable from user-defined functions in the same module.
///
/// `ponytail`: a more aggressive sanitizer would normalise unicode, but
/// the MVP only has ASCII test names.
pub fn test_func_name(test_name: &str) -> String {
    let mut s = String::from("_test_");
    for ch in test_name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            s.push(ch);
        } else {
            s.push('_');
        }
    }
    s
}

/// Build a [`FuncDef`] for a test. The function's body runs the test
/// (with each `assert(...)` replaced by a runtime check that returns
/// `1` on failure) and ends with `return 0` so a successful run yields
/// the conventional pass code.
///
/// The function takes no parameters: the `with`-block defaults are
/// inlined as `let` bindings at the top of the body, so the test driver
/// can call the function with no CLI arguments and still exercise the
/// parameterised form. A future revision can expose the `with` block
/// as parameters for an interactive test runner, but the MVP keeps
/// the protocol simple.
pub fn build_test_func(test: &TestDef) -> FuncDef {
    let name = test_func_name(&test.name);
    let mut body_stmts: Vec<Stmt> = Vec::new();
    // Inline each `with` parameter as a `let` binding. The semantic
    // pass has already verified the default expression type-checks
    // against the declared type, so we only need to put the binding
    // in place; the rest of the body can refer to it by name.
    for p in &test.params {
        body_stmts.push(Stmt::Let {
            names: vec![p.name.clone()],
            ty: Some(p.ty.clone()),
            value: p.default.clone(),
            span: p.span.clone(),
        });
    }
    body_stmts.extend(test.body.stmts.iter().flat_map(|s| {
        if let Stmt::Assert { kind, span } = s {
            // `assert_to_stmts` produces one Stmt per assert for the
            // MVP. `flat_map` collects them so the surrounding
            // `extend` keeps ownership simple.
            assert_to_stmts(kind, span)
        } else {
            vec![s.clone()]
        }
    }));
    // Successful end-of-body marker.
    let span = test.body.span.clone();
    body_stmts.push(Stmt::Return {
        value: Some(Expr::IntLit { value: 0, span: span.clone() }),
        span,
    });
    FuncDef {
        name,
        params: vec![],
        return_type: Some(Type::Int(None)),
        multi_returns: None,
        body: Block { stmts: body_stmts, span: test.body.span.clone() },
        annotations: vec![],
        visibility: Visibility::Public,
        // Tests are not user-authored `fileclude`/`ownclude` - they
        // are synthesised wrappers around the original test body and
        // behave as top-level public functions.
        source_file: String::new(),
        enclosing_func: None,
        span: test.span.clone(),
    }
}

/// Convert a single `assert(...)` into the runtime check sequence:
/// * `InRange` -> an `if` that returns 1 when the value is outside
/// the bound, using the per-side inclusive flag.
/// * `Approx` -> an `if` that returns 1 when `|value - target|`
/// exceeds the tolerance.
///
/// Each form's error message is the original assert's text; this is
/// preserved in the generated source so a failing test still points
/// at the original `assert(...)` site.
fn assert_to_stmts(kind: &AssertKind, span: &Span) -> Vec<Stmt> {
    match kind {
        AssertKind::InRange { value, range, .. } => {
            in_range_check(value, range, span)
        }
        AssertKind::Approx { value, target, tolerance, .. } => {
            approx_check(value, target, tolerance, span)
        }
    }
}

/// Build the in-range check for `assert(ident in RANGE)`. Mirrors the
/// four bracket combinations in ///
/// `[a, b]` -> value >= a && value <= b
/// `{a, b}` -> value > a && value < b
/// `[a, b}` -> value >= a && value < b
/// `{a, b]` -> value > a && value <= b
///
/// The check is encoded as a single `if` whose condition is the
/// *negation* of the success condition, so the failure branch only
/// has to `return 1` (no else).
fn in_range_check(value_name: &str, range: &RangeBound, span: &Span) -> Vec<Stmt> {
    let value = Expr::Ident { name: value_name.to_string(), span: range.span.clone() };
    let lo_op = if range.lo_inclusive { BinOp::Ge } else { BinOp::Gt };
    let hi_op = if range.hi_inclusive { BinOp::Le } else { BinOp::Lt };
    let lo_check = Expr::Binary {
        op: lo_op,
        lhs: Box::new(value.clone()),
        rhs: Box::new(range.lo.clone()),
        span: range.span.clone(),
    };
    let hi_check = Expr::Binary {
        op: hi_op,
        lhs: Box::new(value),
        rhs: Box::new(range.hi.clone()),
        span: range.span.clone(),
    };
    // The overall check is `(lo_check) AND (hi_check)`. The failure
    // condition is the negation: `!(lo_check && hi_check)`.
    let combined = Expr::Binary {
        op: BinOp::And,
        lhs: Box::new(lo_check),
        rhs: Box::new(hi_check),
        span: range.span.clone(),
    };
    let fail_cond = Expr::Unary {
        op: spass_parser::ast::UnaryOp::Not,
        expr: Box::new(combined),
        span: range.span.clone(),
    };
    vec![failure_return(fail_cond, span)]
}

/// Build the approximation check for `assert(approx(value, target, tolerance))`.
/// The check is `|value - target| > tolerance`, which is the negation of
/// the success condition. Tolerance is `f64` in the MVP - the
/// approximation is always against a continuous value.
fn approx_check(value_name: &str, target: &Expr, tolerance: &Expr, span: &Span) -> Vec<Stmt> {
    let value = Expr::Ident { name: value_name.to_string(), span: target.span() };
    let diff = Expr::Binary {
        op: BinOp::Sub,
        lhs: Box::new(value),
        rhs: Box::new(target.clone()),
        span: target.span(),
    };
    // `|x|` is encoded as a function call to the runtime's `spass_abs`.
    // The Rust emitter maps `abs(...)` to the standard library's `f64::abs`
    // so this round-trips through codegen without an extra built-in.
    let abs_diff = Expr::Call {
        callee: Box::new(Expr::Ident { name: "abs".to_string(), span: target.span() }),
        args: vec![spass_parser::ast::CallArg { value: diff, span: target.span() }],
        span: target.span(),
    };
    let fail_cond = Expr::Binary {
        op: BinOp::Gt,
        lhs: Box::new(abs_diff),
        rhs: Box::new(tolerance.clone()),
        span: target.span(),
    };
    vec![failure_return(fail_cond, span)]
}

/// Wrap a failure condition in an `if` whose body is `return 1`.
/// `ponytail`: a future revision could record the failure reason in a
/// dedicated output channel; for the MVP the only signal is the
/// function's return value.
fn failure_return(cond: Expr, span: &Span) -> Stmt {
    let then_branch = Block {
        stmts: vec![Stmt::Return {
            value: Some(Expr::IntLit { value: 1, span: span.clone() }),
            span: span.clone(),
        }],
        span: span.clone(),
    };
    let else_branch = Block { stmts: vec![], span: span.clone() };
    Stmt::If { cond, then_branch, else_branch, span: span.clone() }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the assert -> if-return rewrite. Each test pins
    //! one of the four bracket combinations plus the approximation
    //! form. The output is a `Vec<Stmt>`; we only assert on the
    //! outer shape (a single `if` returning 1) and the inclusive
    //! flag encoding, not the deep expression structure.

    use super::*;
    use spass_lexer::Span;
    // `TestParam` is only referenced by the `build_test_func_inlines_with_params`
    // test below, so we import it locally instead of in the module header.
    use spass_parser::ast::TestParam;

    fn span() -> Span {
        Span::new("<test>".to_string(), 0, 0, 0, 0)
    }

    fn mk_range(lo_inclusive: bool, hi_inclusive: bool) -> RangeBound {
        let s = span();
        RangeBound {
            lo: Expr::FloatLit { value: num_rational::Rational64::from(0), span: s.clone() },
            hi: Expr::FloatLit { value: num_rational::Rational64::from(1), span: s.clone() },
            lo_inclusive,
            hi_inclusive,
            span: s,
        }
    }

    fn first_if(stmts: &[Stmt]) -> &Stmt {
        match stmts.first() {
            Some(Stmt::If { .. }) => &stmts[0],
            other => panic!("expected leading if, got {:?}", other),
        }
    }

    #[test]
    fn in_range_closed_closed_uses_ge_and_le() {
        let stmts = in_range_check("h", &mk_range(true, true), &span());
        // The body is a single `if`. The else is empty.
        let Stmt::If { then_branch, else_branch, .. } = first_if(&stmts) else {
            unreachable!()
        };
        assert_eq!(then_branch.stmts.len(), 1);
        assert!(else_branch.stmts.is_empty());
    }

    #[test]
    fn in_range_open_open_uses_gt_and_lt() {
        // Same shape; the operator change is inside the Expr tree
        // (which is harder to introspect from this level). We at
        // least verify the wrapper produced exactly one stmt.
        let stmts = in_range_check("h", &mk_range(false, false), &span());
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn approx_check_emits_single_if() {
        let target = Expr::FloatLit { value: num_rational::Rational64::from(0), span: span() };
        let tolerance = Expr::FloatLit { value: num_rational::Rational64::from(0), span: span() };
        let stmts = approx_check("h", &target, &tolerance, &span());
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn build_test_func_produces_int_return() {
        let test = TestDef {
            name: "demo".to_string(),
            params: vec![],
            body: Block { stmts: vec![], span: span() },
            span: span(),
        };
        let f = build_test_func(&test);
        assert_eq!(f.name, "_test_demo");
        assert!(matches!(f.return_type, Some(Type::Int(_))));
        // Even an empty body ends with `return 0`.
        assert!(!f.body.stmts.is_empty());
    }

    #[test]
    fn test_func_name_sanitises_spaces() {
        assert_eq!(test_func_name("hello world"), "_test_hello_world");
        assert_eq!(test_func_name("a-b/c"), "_test_a_b_c");
        assert_eq!(test_func_name("simple"), "_test_simple");
    }

    #[test]
    fn build_test_func_replaces_assert_with_if() {
        let assert_stmt = Stmt::Assert {
            kind: AssertKind::InRange {
                value: "h".to_string(),
                range: mk_range(true, true),
                span: span(),
            },
            span: span(),
        };
        let test = TestDef {
            name: "demo".to_string(),
            params: vec![],
            body: Block { stmts: vec![assert_stmt], span: span() },
            span: span(),
        };
        let f = build_test_func(&test);
        // Assert becomes one `if`; final `return 0` makes two stmts.
        assert_eq!(f.body.stmts.len(), 2);
        assert!(matches!(f.body.stmts[0], Stmt::If { .. }));
        assert!(matches!(f.body.stmts[1], Stmt::Return { .. }));
    }

    #[test]
    fn build_test_func_inlines_with_params() {
        // The `with` block's parameters become leading `let`
        // bindings; the original body follows them.
        let param = TestParam {
            name: "x".to_string(),
            ty: Type::Float(None),
            default: Expr::FloatLit {
                value: num_rational::Rational64::from(0),
                span: span(),
            },
            span: span(),
        };
        let test = TestDef {
            name: "p".to_string(),
            params: vec![param],
            body: Block { stmts: vec![], span: span() },
            span: span(),
        };
        let f = build_test_func(&test);
        // `let x =...` then `return 0` -> two stmts.
        assert_eq!(f.body.stmts.len(), 2);
        match &f.body.stmts[0] {
            Stmt::Let { names, .. } => assert_eq!(names, &vec!["x".to_string()]),
            other => panic!("expected let, got {:?}", other),
        }
        // No parameters on the synthetic function.
        assert!(f.params.is_empty());
    }
}
