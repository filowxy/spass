//! Lowers `if_present` and `@dispatch` in a function body before emission.

use spass_parser::ast::{Block, Stmt};
use spass_semantic::{SemanticError, TypedModule};

use crate::dispatch;
use crate::if_present::Variant;

/// Turn a source body into the body of one `if_present` variant.
///
/// The result has no `if_present` statement and no `@dispatch` statement
/// left: an `if_present` became the statements of the branch that
/// matches the variant, and a `@dispatch` became the chain of calls its
/// left-fold expands to.
pub fn prepare_body(
    module: &TypedModule,
    stmts: &[Stmt],
    variant: Variant,
) -> Result<Vec<Stmt>, SemanticError> {
    let mut out = Vec::with_capacity(stmts.len());
    for s in stmts {
        prepare_one(module, s, variant, &mut out)?;
    }
    Ok(out)
}

fn prepare_block(
    module: &TypedModule,
    block: &Block,
    variant: Variant,
) -> Result<Block, SemanticError> {
    Ok(Block {
        stmts: prepare_body(module, &block.stmts, variant)?,
        span: block.span.clone(),
    })
}

fn prepare_one(
    module: &TypedModule,
    stmt: &Stmt,
    variant: Variant,
    out: &mut Vec<Stmt>,
) -> Result<(), SemanticError> {
    match stmt {
        Stmt::IfPresent { present_branch, absent_branch, .. } => {
            // Pick the branch for this variant. The absent path is
            // `None` when the user wrote no `@absent {... }` block; in
            // that case the absent variant falls through to whatever
            // follows the `if_present`.
            let branch: Option<&Block> = match variant {
                Variant::Present => Some(present_branch),
                Variant::Absent => absent_branch.as_ref(),
            };
            if let Some(b) = branch {
                for inner in &b.stmts {
                    prepare_one(module, inner, variant, out)?;
                }
            }
        }
        Stmt::Dispatch { decl, apply, span } => {
            // the dispatch expands into a left-fold over
            // the matched functions. Each step writes its result back to
            // the user's target, so the next step's carry is the
            // previous step's output.
            let expansions = dispatch::expand(
                module,
                &decl.tags,
                &decl.bind,
                &apply.call,
                apply.target.as_deref(),
            )?;
            for (target, call, _) in expansions {
                out.push(Stmt::Assign { name: target, value: call, span: span.clone() });
            }
        }
        Stmt::If { cond, then_branch, else_branch, span } => {
            let then_branch = prepare_block(module, then_branch, variant)?;
            let else_branch = prepare_block(module, else_branch, variant)?;
            out.push(Stmt::If {
                cond: cond.clone(),
                then_branch,
                else_branch,
                span: span.clone(),
            });
        }
        Stmt::For { var, iter, body, span } => {
            let body = prepare_block(module, body, variant)?;
            out.push(Stmt::For {
                var: var.clone(),
                iter: iter.clone(),
                body,
                span: span.clone(),
            });
        }
        Stmt::While { cond, body, span } => {
            let body = prepare_block(module, body, variant)?;
            out.push(Stmt::While {
                cond: cond.clone(),
                body,
                span: span.clone(),
            });
        }
        other => out.push(other.clone()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use spass_lexer::Span;
    use spass_parser::ast::Expr;

    fn int_lit(v: i64) -> Expr {
        Expr::IntLit { value: v, span: Span::dummy() }
    }

    fn let_stmt(name: &str, v: i64) -> Stmt {
        Stmt::Let {
            names: vec![name.to_string()],
            ty: None,
            value: int_lit(v),
            span: Span::dummy(),
        }
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

    fn empty_module() -> TypedModule {
        use spass_parser::Parser;
        let module = Parser::parse_source("func main(): float { return 0.0 }", "<test>").expect("parse");
        spass_semantic::analyze(&module).expect("analyse")
    }

    #[test]
    fn the_present_variant_keeps_the_present_branch() {
        let module = empty_module();
        let out = prepare_body(
            &module,
            &[if_present(vec![let_stmt("a", 1)], Some(vec![let_stmt("b", 2)]))],
            Variant::Present,
        )
        .expect("prepare");
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], Stmt::Let { .. }));
    }

    #[test]
    fn the_absent_variant_keeps_the_absent_branch() {
        let module = empty_module();
        let out = prepare_body(
            &module,
            &[if_present(vec![let_stmt("a", 1)], Some(vec![let_stmt("b", 2)]))],
            Variant::Absent,
        )
        .expect("prepare");
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], Stmt::Let { .. }));
    }

    /// A body without any compile-time construct passes through
    /// unchanged.
    #[test]
    fn a_plain_body_passes_through() {
        let module = empty_module();
        let body = vec![let_stmt("a", 1)];
        let out = prepare_body(&module, &body, Variant::Present).expect("prepare");
        assert_eq!(out, body);
    }
}
