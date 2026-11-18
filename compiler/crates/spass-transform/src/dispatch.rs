//! Lowers validated `@dispatch` statements into explicit calls.

use spass_lexer::Span;
use spass_parser::ast::{Expr, TagValue};
use spass_semantic::dispatch::{ResolvedBind, ResolvedDispatch, topo_sort};
use spass_semantic::{SemanticError, TypedFunc, TypedModule};

/// Expand a dispatch into a vector of `(target, call_expr, span)`
/// triples. Each triple is one matched function in the sorted order;
/// the codegen layer turns it into a call statement.
///
/// `apply_call` is the call expression from the original
/// `Stmt::Dispatch` - its first N-1 args are the "static" head, its
/// last arg is the carry. The expanded calls share those heads; the
/// carry is threaded through the left-fold.
pub fn expand(
    module: &TypedModule,
    decl_tags: &spass_parser::ast::TagSelector,
    decl_bind: &[spass_parser::ast::DispatchBind],
    apply_call: &Expr,
    target: Option<&str>,
) -> Result<Vec<(String, Expr, Span)>, SemanticError> {
    // Re-resolve the dispatch to get the sorted match list. The
    // semantic pass has already validated everything, so the
    // resolve_dispatch call here just regenerates the structure
    // for codegen.
    let resolved = resolve_for_codegen(module, decl_tags, decl_bind, apply_call)?;
    let target_name = target
        .ok_or_else(|| SemanticError::TypeMismatch {
            message: "@dispatch requires an assignment target (e.g. `h = apply(...)`)".into(),
            span: apply_call.span(),
        })?
        .to_string();

    // Build the call list.
    let mut out = Vec::new();
    let apply_args = &resolved.apply_args;
    if apply_args.is_empty() {
        return Err(SemanticError::TypeMismatch {
            message: "apply must have at least one argument (the carry)".into(),
            span: apply_call.span(),
        });
    }
    // The static head is all apply args except the last (the carry).
    let static_head = &apply_args[..apply_args.len() - 1];

    for (i, f) in resolved.ordered.iter().enumerate() {
        // The carry: the last apply arg. The user wrote
        // `h = apply(pos, h)`; the lhs `h` is the carry. The
        // expansion produces one call per matched function in the
        // sorted order, with the same static head and carry. Each
        // call writes its result back to the user's target, so the
        // next call's carry input is the previous call's output.
        let _ = i;
        let call_expr = build_call(f, static_head, &apply_args[apply_args.len() - 1], &resolved.bind);
        out.push((target_name.clone(), call_expr, decl_span(decl_tags)));
    }
    Ok(out)
}

fn decl_span(_sel: &spass_parser::ast::TagSelector) -> Span {
    Span::dummy()
}

/// Build the call expression `<fn>(static_args..., carry, <bind args...>)`
/// where bind args are resolved for this specific function.
fn build_call(
    f: &TypedFunc,
    static_head: &[Expr],
    carry: &Expr,
    bind: &[ResolvedBind],
) -> Expr {
    // The call's args: static_head ++ [carry] ++ bind values for `f`.
    // For each bind entry, look up the function's param at the same
    // name. If the function has that param, append the value; if not,
    // skip (W002 - the user gets a warning at the dispatch site but
    // the codegen still works because the param is unused).
    let mut arg_exprs: Vec<Expr> = static_head.to_vec();
    arg_exprs.push(carry.clone());
    for b in bind.iter() {
        let bind_param = match b {
            ResolvedBind::Literal { param, .. } | ResolvedBind::TagValue { param, .. } => param,
        };
        let f_has_param = f.params.iter().any(|p| p.name == *bind_param);
        if !f_has_param {
            continue;
        }
        let val_expr: Expr = match b {
            ResolvedBind::Literal { value, .. } => value.clone(),
            ResolvedBind::TagValue { value, .. } => tag_value_to_expr(value),
        };
        arg_exprs.push(val_expr);
    }
    Expr::Call {
        callee: Box::new(Expr::Ident { name: f.name.clone(), span: Span::dummy() }),
        args: arg_exprs.into_iter().map(|v| spass_parser::ast::CallArg { value: v, span: Span::dummy() }).collect(),
        span: Span::dummy(),
    }
}

/// Convert a `TagValue` to a Spass expression literal. The codegen
/// then handles it like any other literal at the call site.
fn tag_value_to_expr(v: &TagValue) -> Expr {
    match v {
        TagValue::Number(r) => Expr::FloatLit { value: r.clone(), span: Span::dummy() },
        TagValue::String(s) => Expr::StringLit { value: s.clone(), span: Span::dummy() },
        TagValue::Bool(b) => Expr::BoolLit { value: *b, span: Span::dummy() },
    }
}

/// Re-run dispatch resolution for codegen. Same logic as the
/// semantic pass's check; we duplicate it here to avoid making
/// the semantic pass produce side artifacts.
fn resolve_for_codegen(
    module: &TypedModule,
    decl_tags: &spass_parser::ast::TagSelector,
    decl_bind: &[spass_parser::ast::DispatchBind],
    apply_call: &Expr,
) -> Result<ResolvedDispatch, SemanticError> {
    // 1. Match
    let matched = match_selector(module, decl_tags);
    if matched.is_empty() {
        return Err(SemanticError::DispatchNoMatch { span: Span::dummy() });
    }
    // 2. Apply args
    let apply_args: Vec<Expr> = match apply_call {
        Expr::Call { args, .. } => args.iter().map(|a| a.value.clone()).collect(),
        _ => {
            return Err(SemanticError::TypeMismatch {
                message: "apply must be a function call".into(),
                span: apply_call.span(),
            });
        }
    };
    if apply_args.is_empty() {
        return Err(SemanticError::TypeMismatch {
            message: "apply must have at least one argument (the carry)".into(),
            span: apply_call.span(),
        });
    }
    let static_arg_count = apply_args.len() - 1;
    for f in &matched {
        for (i, p) in f.params.iter().enumerate() {
            if i < static_arg_count {
                continue;
            }
            if i == static_arg_count {
                // Carry position: satisfied by the last apply arg, not by a bind.
                continue;
            }
            if !decl_bind.iter().any(|b| b.param == p.name) {
                return Err(SemanticError::DispatchBindMissing {
                    param: p.name.clone(),
                    span: apply_call.span(),
                });
            }
        }
    }
    let ordered = topo_sort(matched)?;
    let bind: Vec<ResolvedBind> = decl_bind
        .iter()
        .map(|b| match &b.source {
            spass_parser::ast::DispatchBindSource::Literal(e) => ResolvedBind::Literal {
                param: b.param.clone(),
                value: e.clone(),
                span: b.span.clone(),
            },
            spass_parser::ast::DispatchBindSource::TagRef(key, span) => {
                let value = ordered
                    .iter()
                    .find_map(|f| {
                        for ann in &f.original.annotations {
                            if let spass_parser::ast::Annotation::Tag { items, .. } = ann {
                                for it in items {
                                    if let spass_parser::ast::TagItem::Valued(k, v, _) = it {
                                        if k == key {
                                            return Some(v.clone());
                                        }
                                    }
                                }
                            }
                        }
                        None
                    })
                    .unwrap_or(TagValue::Number(num_rational::Rational64::from_integer(0)));
                ResolvedBind::TagValue { param: b.param.clone(), value, span: span.clone() }
            }
        })
        .collect();
    Ok(ResolvedDispatch { ordered, apply_args, bind, span: Span::dummy() })
}

/// Local re-implementation of `match_selector` (avoids a self-import
/// cycle by depending only on the public types). Identical to the
/// version in `dispatch::match_selector`.
fn match_selector(
    module: &TypedModule,
    selector: &spass_parser::ast::TagSelector,
) -> Vec<TypedFunc> {
    use std::collections::HashSet;
    let (bare, valued) = split_selector(selector);
    module
        .functions
        .iter()
        .filter(|f| {
            let func_bares: HashSet<&str> = f.tags.iter().map(|s| s.as_str()).collect();
            let bare_ok = match selector {
                spass_parser::ast::TagSelector::Or(_) => {
                    bare.iter().any(|b| func_bares.contains(b.as_str()))
                }
                spass_parser::ast::TagSelector::And(_) => {
                    bare.iter().all(|b| func_bares.contains(b.as_str()))
                }
            };
            if !bare_ok {
                return false;
            }
            for (k, v) in &valued {
                let mut found = false;
                for ann in &f.original.annotations {
                    if let spass_parser::ast::Annotation::Tag { items, .. } = ann {
                        for it in items {
                            if let spass_parser::ast::TagItem::Valued(kk, vv, _) = it {
                                if kk == k && tag_value_eq(vv, v) {
                                    found = true;
                                }
                            }
                        }
                    }
                }
                if !found {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect()
}

fn split_selector(
    sel: &spass_parser::ast::TagSelector,
) -> (Vec<String>, Vec<(String, TagValue)>) {
    let items: &[spass_parser::ast::TagItem] = match sel {
        spass_parser::ast::TagSelector::Or(items)
        | spass_parser::ast::TagSelector::And(items) => items,
    };
    let mut bare = Vec::new();
    let mut valued = Vec::new();
    for it in items {
        match it {
            spass_parser::ast::TagItem::Bare(n) => bare.push(n.clone()),
            spass_parser::ast::TagItem::Valued(k, v, _) => valued.push((k.clone(), v.clone())),
        }
    }
    (bare, valued)
}

fn tag_value_eq(a: &TagValue, b: &TagValue) -> bool {
    match (a, b) {
        (TagValue::Number(x), TagValue::Number(y)) => x == y,
        (TagValue::String(x), TagValue::String(y)) => x == y,
        (TagValue::Bool(x), TagValue::Bool(y)) => x == y,
        _ => false,
    }
}
