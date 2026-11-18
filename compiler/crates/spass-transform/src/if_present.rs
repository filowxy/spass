//! Lowers `if_present` branches into concrete function variants.

use spass_parser::ast::{Block, Stmt};

/// Which variant of an `if_present` function we're emitting.
///
/// `Present` - every `if_present` is replaced by its
/// `present_branch`. Used when the caller supplied the gated
/// parameter.
///
/// `Absent` - every `if_present` is replaced by its
/// `absent_branch` (or removed, if no `@absent` was given). Used
/// when the caller omitted the gated parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Present,
    Absent,
}

/// Transform a statement list for one variant of an `if_present`
/// function. The walk is recursive so `if_present` inside nested
/// `if` / `for` / `while` blocks is also resolved. Anything else
/// passes through unchanged.
///
/// `ponytail`: the transform is a one-pass fold; statements are
/// emitted in source order. The function body is a `Vec<Stmt>` and
/// the result is also a `Vec<Stmt>` - same shape, no extra
/// allocation beyond the new vec.
pub fn transform_stmts(stmts: &[Stmt], variant: Variant) -> Vec<Stmt> {
    let mut out = Vec::with_capacity(stmts.len());
    for s in stmts {
        transform_one_into(s, variant, &mut out);
    }
    out
}

fn transform_one_into(s: &Stmt, variant: Variant, out: &mut Vec<Stmt>) {
    match s {
        Stmt::IfPresent { present_branch, absent_branch, .. } => {
            // Pick the branch for this variant. The absent path is
            // `None` when the user wrote no `@absent {... }` block;
            // in that case the absent variant falls through and the
            // `if_present` is removed entirely.
            let branch: Option<&Block> = match variant {
                Variant::Present => Some(present_branch),
                Variant::Absent => absent_branch.as_ref(),
            };
            if let Some(b) = branch {
                for inner in &b.stmts {
                    transform_one_into(inner, variant, out);
                }
            }
        }
        Stmt::If { cond, then_branch, else_branch, span } => {
            let new_then = Block {
                stmts: transform_stmts(&then_branch.stmts, variant),
                span: then_branch.span.clone(),
            };
            let new_else = Block {
                stmts: transform_stmts(&else_branch.stmts, variant),
                span: else_branch.span.clone(),
            };
            out.push(Stmt::If {
                cond: cond.clone(),
                then_branch: new_then,
                else_branch: new_else,
                span: span.clone(),
            });
        }
        Stmt::For { var, iter, body, span } => {
            let new_body = Block {
                stmts: transform_stmts(&body.stmts, variant),
                span: body.span.clone(),
            };
            out.push(Stmt::For {
                var: var.clone(),
                iter: iter.clone(),
                body: new_body,
                span: span.clone(),
            });
        }
        Stmt::While { cond, body, span } => {
            let new_body = Block {
                stmts: transform_stmts(&body.stmts, variant),
                span: body.span.clone(),
            };
            out.push(Stmt::While {
                cond: cond.clone(),
                body: new_body,
                span: span.clone(),
            });
        }
        other => out.push(other.clone()),
    }
}

/// Apply the `if_present` transform to a function's body and return
/// a fresh `TypedFunc`-shaped body. The original `TypedFunc` is
/// unchanged; callers swap the body in via a temporary `FuncDef`
/// clone so the existing `emit_function` / `emit_rust_function`
/// paths don't need to know about variants.
pub fn transform_body(stmts: &[Stmt], variant: Variant) -> Vec<Stmt> {
    transform_stmts(stmts, variant)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use spass_parser::ast::Expr;
    use spass_lexer::Span;

    fn int_lit(v: i64) -> Expr {
        Expr::IntLit { value: v, span: Span::dummy() }
    }

    fn ident(name: &str) -> Expr {
        Expr::Ident { name: name.to_string(), span: Span::dummy() }
    }

    fn block(stmts: Vec<Stmt>) -> Block {
        Block { stmts, span: Span::dummy() }
    }

    fn if_present(present: Vec<Stmt>, absent: Option<Vec<Stmt>>) -> Stmt {
        Stmt::IfPresent {
            param: "x".to_string(),
            present_branch: block(present),
            absent_branch: absent.map(block),
            span: Span::dummy(),
        }
    }

    fn let_stmt(name: &str, v: i64) -> Stmt {
        Stmt::Let {
            names: vec![name.to_string()],
            ty: None,
            value: int_lit(v),
            span: Span::dummy(),
        }
    }

    fn return_stmt() -> Stmt {
        Stmt::Return { value: Some(int_lit(0)), span: Span::dummy() }
    }

    fn stmts_kind(stmts: &[Stmt]) -> Vec<&'static str> {
        stmts.iter().map(|s| match s {
            Stmt::Let { .. } => "let",
            Stmt::Return { .. } => "return",
            Stmt::Expr(_, _) => "expr",
            Stmt::IfPresent { .. } => "if_present",
            Stmt::If { .. } => "if",
            Stmt::For { .. } => "for",
            Stmt::While { .. } => "while",
            _ => "other",
        }).collect()
    }

    #[test]
    fn present_replaces_with_present_branch() {
        let stmts = vec![
            let_stmt("a", 1),
            if_present(vec![let_stmt("b", 2)], Some(vec![let_stmt("c", 3)])),
            return_stmt(),
        ];
        let out = transform_stmts(&stmts, Variant::Present);
        // present branch -> let b, original fall-through
        assert_eq!(stmts_kind(&out), vec!["let", "let", "return"]);
    }

    #[test]
    fn absent_replaces_with_absent_branch() {
        let stmts = vec![
            let_stmt("a", 1),
            if_present(vec![let_stmt("b", 2)], Some(vec![let_stmt("c", 3)])),
            return_stmt(),
        ];
        let out = transform_stmts(&stmts, Variant::Absent);
        // absent branch -> let c
        assert_eq!(stmts_kind(&out), vec!["let", "let", "return"]);
    }

    #[test]
    fn absent_without_absent_block_removes_if_present() {
        let stmts = vec![
            let_stmt("a", 1),
            if_present(vec![let_stmt("b", 2)], None),
            return_stmt(),
        ];
        let out = transform_stmts(&stmts, Variant::Absent);
        // No @absent -> the if_present is just dropped; fall-through is normal
        assert_eq!(stmts_kind(&out), vec!["let", "return"]);
    }

    #[test]
    fn nested_if_present_inside_if() {
        let inner = if_present(vec![let_stmt("b", 2)], Some(vec![let_stmt("c", 3)]));
        let outer_if = Stmt::If {
            cond: ident("cond"),
            then_branch: block(vec![inner]),
            else_branch: block(vec![return_stmt()]),
            span: Span::dummy(),
        };
        let out = transform_stmts(&[outer_if], Variant::Present);
        // Outer If survives; its then-branch is rewritten.
        match &out[0] {
            Stmt::If { then_branch, else_branch, .. } => {
                assert_eq!(stmts_kind(&then_branch.stmts), vec!["let"]);
                assert_eq!(stmts_kind(&else_branch.stmts), vec!["return"]);
            }
            _ => panic!("expected If"),
        }
    }

    #[test]
    fn no_if_present_is_passthrough() {
        let stmts = vec![let_stmt("a", 1), return_stmt()];
        let present = transform_stmts(&stmts, Variant::Present);
        let absent = transform_stmts(&stmts, Variant::Absent);
        assert_eq!(stmts_kind(&present), vec!["let", "return"]);
        assert_eq!(stmts_kind(&absent), vec!["let", "return"]);
    }
}
