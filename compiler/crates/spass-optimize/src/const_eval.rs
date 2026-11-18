//! Evaluates constant expressions at compile time.

use num_rational::Rational64;
use spass_parser::ast::{BinOp, Expr};

/// A value the evaluator knows at compile time.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    /// An exact integer.
    Int(i64),
    /// An exact rational. Spass literals are rationals, so `0.1` is
    /// `1/10` and not the nearest `f64` - the folding therefore does not
    /// drift the way a `f64` fold would.
    Float(Rational64),
    /// A boolean.
    Bool(bool),
}

impl ConstValue {
    /// The value as a rational, for the arithmetic below.
    pub fn as_rational(&self) -> Option<Rational64> {
        match self {
            ConstValue::Int(v) => Some(Rational64::from(*v)),
            ConstValue::Float(v) => Some(*v),
            ConstValue::Bool(_) => None,
        }
    }

    /// The value as an integer, when it is one.
    ///
    /// A boolean is *not* an integer here: `true` is a distinct value
    /// in the language, and folding it into `1` would let `true + 1`
    /// compile to `2`.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            ConstValue::Int(v) => Some(*v),
            _ => None,
        }
    }

    /// The value as a boolean, when it is one.
    ///
    /// Only a boolean is one: an integer is not a boolean in this
    /// language, and folding `1` into `true` would let `!0` compile to
    /// `true`.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            ConstValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Build the AST expression that stands for this value.
    pub fn to_expr(&self, span: spass_lexer::Span) -> Expr {
        match self {
            ConstValue::Int(v) => Expr::IntLit { value: *v, span },
            ConstValue::Float(v) => Expr::FloatLit { value: *v, span },
            ConstValue::Bool(b) => Expr::BoolLit { value: *b, span },
        }
    }
}

/// Evaluate `expr` to a compile-time constant, if it is one.
///
/// `depth` bounds the recursion so a pathological expression cannot blow
/// the stack; `max_depth` is the configured limit.
///
/// A binary expression is folded only when both operands fold and the
/// operator is one the evaluator implements. Division is excluded on
/// purpose (see the module comment).
pub fn eval(expr: &Expr, depth: usize, max_depth: usize) -> Option<ConstValue> {
    if depth > max_depth {
        return None;
    }
    match expr {
        Expr::IntLit { value, .. } => Some(ConstValue::Int(*value)),
        Expr::FloatLit { value, .. } => Some(ConstValue::Float(*value)),
        Expr::BoolLit { value, .. } => Some(ConstValue::Bool(*value)),
        Expr::Binary { op, lhs, rhs, .. } => {
            let l = eval(lhs, depth + 1, max_depth)?;
            let r = eval(rhs, depth + 1, max_depth)?;
            fold_binary(*op, &l, &r)
        }
        // `Unary` only has `-` and `!` in the language; both are exact.
        Expr::Unary { op, expr: inner, .. } => {
            use spass_parser::ast::UnaryOp;
            match op {
                // The negation of an integer stays an integer: an
                // integer overflow has to stay visible at compile time
                // rather than turn into a float.
                UnaryOp::Neg => match eval(inner, depth + 1, max_depth)? {
                    ConstValue::Int(v) => Some(ConstValue::Int(v.checked_neg()?)),
                    other => Some(ConstValue::Float(-other.as_rational()?)),
                },
                UnaryOp::Not => Some(ConstValue::Bool(!eval(inner, depth + 1, max_depth)?.as_bool()?)),
            }
        }
        // Everything else - calls, channel I/O, awaits, casts - is not a
        // compile-time constant.
        _ => None,
    }
}

/// Fold one binary operation over two constants.
fn fold_binary(op: BinOp, l: &ConstValue, r: &ConstValue) -> Option<ConstValue> {
    // The comparison operators are exact on both integer and rational
    // operands, and they are what a fold of `if` conditions needs.
    match op {
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            let (lv, rv) = (l.as_rational()?, r.as_rational()?);
            let b = match op {
                BinOp::Eq => lv == rv,
                BinOp::Ne => lv != rv,
                BinOp::Lt => lv < rv,
                BinOp::Le => lv <= rv,
                BinOp::Gt => lv > rv,
                BinOp::Ge => lv >= rv,
                _ => unreachable!(),
            };
            return Some(ConstValue::Bool(b));
        }
        BinOp::And | BinOp::Or => {
            let (lv, rv) = (l.as_bool()?, r.as_bool()?);
            return Some(ConstValue::Bool(if op == BinOp::And {
                lv && rv
            } else {
                lv || rv
            }));
        }
        _ => {}
    }

    // Arithmetic. Division is excluded: a fixed-point division is not
    // allowed at all and a float division rounds,
    // so a folded quotient would not be the number the runtime produces.
    if op == BinOp::Div {
        return None;
    }
    // Integer arithmetic stays integer so an integer overflow is visible
    // at compile time rather than silently wrapping.
    if let (Some(li), Some(ri)) = (l.as_int(), r.as_int()) {
        let v = match op {
            BinOp::Add => li.checked_add(ri)?,
            BinOp::Sub => li.checked_sub(ri)?,
            BinOp::Mul => li.checked_mul(ri)?,
            BinOp::Pow => li.checked_pow(ri as u32)?,
            _ => return None,
        };
        return Some(ConstValue::Int(v));
    }
    let (lv, rv) = (l.as_rational()?, r.as_rational()?);
    let v = match op {
        BinOp::Add => lv + rv,
        BinOp::Sub => lv - rv,
        BinOp::Mul => lv * rv,
        BinOp::Pow => {
            // A negative or fractional exponent is not rational in
            // general; only the integer case folds.
            if *rv.denom() != 1 || *rv.numer() < 0 {
                return None;
            }
            lv.pow(*rv.numer() as i32)
        }
        _ => return None,
    };
    Some(ConstValue::Float(v))
}

/// Whether an expression has any side effect the optimiser must preserve.
///
/// A call, a channel operation, an await, a spawn or a parallel all
/// have one. The optimiser uses this to decide whether a binding's
/// initialiser may be inlined at its use site - inlining a
/// side-effecting initialiser would change how many times it runs.
pub fn has_side_effect(expr: &Expr) -> bool {
    match expr {
        Expr::Call { .. }
        | Expr::SpasioInput { .. }
        | Expr::SpasioOutput { .. }
        | Expr::NativeCall { .. }
        | Expr::Await { .. }
        | Expr::Spawn { .. }
        | Expr::Parallel { .. }
        | Expr::Send { .. }
        | Expr::Broadcast { .. }
        | Expr::Receive { .. } => true,
        Expr::Binary { lhs, rhs, .. } => has_side_effect(lhs) || has_side_effect(rhs),
        Expr::Unary { expr, .. } => has_side_effect(expr),
        Expr::FieldAccess { target, .. } => has_side_effect(target),
        Expr::Index { target, index, .. } => has_side_effect(target) || has_side_effect(index),
        Expr::Tuple { elements, .. } | Expr::ListLit { elements, .. } => {
            elements.iter().any(has_side_effect)
        }
        Expr::StructLit { fields, .. } => fields.iter().any(|(_, v)| has_side_effect(v)),
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => has_side_effect(cond)
            || has_side_effect(then_branch)
            || has_side_effect(else_branch),
        Expr::AsType { value, .. }
        | Expr::NarrowCast { value, .. }
        | Expr::Conv { value, .. } => has_side_effect(value),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spass_lexer::Span;

    fn int(v: i64) -> Expr {
        Expr::IntLit { value: v, span: Span::dummy() }
    }

    fn float(v: &str) -> Expr {
        Expr::FloatLit {
            value: spass_parser::parser::parse_rational(v).expect("a valid literal"),
            span: Span::dummy(),
        }
    }

    fn bin(op: BinOp, l: Expr, r: Expr) -> Expr {
        Expr::Binary { op, lhs: Box::new(l), rhs: Box::new(r), span: Span::dummy() }
    }

    #[test]
    fn literals_fold() {
        assert_eq!(eval(&int(7), 0, 8), Some(ConstValue::Int(7)));
        assert_eq!(eval(&float("0.5"), 0, 8), Some(ConstValue::Float(Rational64::new(1, 2))));
    }

    #[test]
    fn integer_arithmetic_folds() {
        assert_eq!(eval(&bin(BinOp::Add, int(2), int(3)), 0, 8), Some(ConstValue::Int(5)));
        assert_eq!(eval(&bin(BinOp::Mul, int(2), int(3)), 0, 8), Some(ConstValue::Int(6)));
        assert_eq!(eval(&bin(BinOp::Sub, int(2), int(3)), 0, 8), Some(ConstValue::Int(-1)));
    }

    /// Rational arithmetic keeps the exact value: `0.1 + 0.2` is `3/10`,
    /// not the `f64` approximation the runtime would compute.
    #[test]
    fn rational_arithmetic_is_exact() {
        let e = bin(BinOp::Add, float("0.1"), float("0.2"));
        assert_eq!(eval(&e, 0, 8), Some(ConstValue::Float(Rational64::new(3, 10))));
    }

    /// A fixed-point division is not allowed at all,
    /// so folding one would produce a number the runtime never produces.
    #[test]
    fn division_does_not_fold() {
        assert_eq!(eval(&bin(BinOp::Div, int(6), int(3)), 0, 8), None);
    }

    /// Integer overflow is visible at compile time rather than wrapping.
    #[test]
    fn integer_overflow_does_not_fold() {
        let big = Expr::IntLit { value: i64::MAX, span: Span::dummy() };
        assert_eq!(eval(&bin(BinOp::Add, big, int(1)), 0, 8), None);
    }

    #[test]
    fn comparisons_fold() {
        assert_eq!(eval(&bin(BinOp::Lt, int(1), int(2)), 0, 8), Some(ConstValue::Bool(true)));
        assert_eq!(eval(&bin(BinOp::Eq, int(1), int(2)), 0, 8), Some(ConstValue::Bool(false)));
    }

    /// The depth limit stops a deep expression from recursing forever.
    #[test]
    fn depth_is_bounded() {
        let mut e = int(1);
        for _ in 0..10 {
            e = bin(BinOp::Add, e, int(1));
        }
        assert_eq!(eval(&e, 0, 8), None);
        assert_eq!(eval(&e, 0, 32), Some(ConstValue::Int(11)));
    }

    /// A call is never folded: it might write to a channel.
    #[test]
    fn calls_do_not_fold() {
        let call = Expr::Call {
            callee: Box::new(Expr::Ident { name: "f".into(), span: Span::dummy() }),
            args: vec![],
            span: Span::dummy(),
        };
        assert_eq!(eval(&call, 0, 8), None);
        assert!(has_side_effect(&call));
    }

    #[test]
    fn pure_expressions_have_no_side_effect() {
        assert!(!has_side_effect(&bin(BinOp::Add, int(1), int(2))));
        assert!(!has_side_effect(&float("0.5")));
    }
}
