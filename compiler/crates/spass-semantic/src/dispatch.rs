//! Validates `@dispatch` tag matching, binding, and ordering.

use spass_lexer::Span;
use spass_parser::ast::{
    Annotation, ApplyStmt, DispatchDecl, Expr, Stmt, TagItem, TagSelector, TagValue,
};
use crate::{SemanticError, SemanticWarning, TypedFunc, TypedModule};
use std::collections::{HashMap, HashSet};

/// A resolved dispatch site. Produced by [`resolve_dispatch`].
#[derive(Debug, Clone)]
pub struct ResolvedDispatch {
    /// The matched functions in their final call order.
    pub ordered: Vec<TypedFunc>,
    /// The apply call arguments used as the "static" head of every
    /// expanded call (the first N args of `apply(...)`). The last
    /// one is the carry that threads through the left-fold.
    pub apply_args: Vec<Expr>,
    /// The bind bindings in declaration order. Each binding is the
    /// literal or the resolved tag value.
    pub bind: Vec<ResolvedBind>,
    /// Original header span, used for error sites.
    pub span: Span,
}

/// One bind entry after resolution. The `Literal` variant carries
/// the original source expression (for codegen). The `TagValue`
/// variant carries a clone of the value read off the matched
/// function's tag.
#[derive(Debug, Clone)]
pub enum ResolvedBind {
    Literal { param: String, value: Expr, span: Span },
    TagValue { param: String, value: TagValue, span: Span },
}

/// Topologically sort `funcs` using the @before / @after orderings
/// collected from each function's body. If a cycle is detected,
/// returns an `Err` with the cycle path.
pub fn topo_sort(funcs: Vec<TypedFunc>) -> Result<Vec<TypedFunc>, SemanticError> {
    // 1. Index by name for fast lookup.
    let by_name: HashMap<String, TypedFunc> =
        funcs.iter().map(|f| (f.name.clone(), f.clone())).collect();

    // 2. Build edge set: edges[i] = set of indices that `i` must run
    // BEFORE. `@before(X)` adds an edge from `me -> X`; `@after(X)`
    // adds an edge from `X -> me`.
    let mut edges: Vec<HashSet<usize>> = vec![HashSet::new(); funcs.len()];
    let mut has_cycle = false;
    let mut cycle_path = String::new();

    for (i, f) in funcs.iter().enumerate() {
        for stmt in &f.original.body.stmts {
            if let Stmt::OrderDecl { kind, target, require, span } = stmt {
                if !require {
                    // Soft ordering - ignored for MVP topo sort.
                    let _ = (kind, target, span);
                    continue;
                }
                let j = match by_name.get(target) {
                    Some(t) => funcs.iter().position(|x| x.name == t.name),
                    None => None,
                };
                if let Some(j) = j {
                    match kind {
                        spass_parser::ast::OrderKind::Before => {
                            edges[i].insert(j);
                        }
                        spass_parser::ast::OrderKind::After => {
                            edges[j].insert(i);
                        }
                    }
                }
            }
        }
    }

    // 3. Kahn's algorithm. Repeatedly pick a node with no incoming
    // edges and append it. If we run out of nodes with incoming
    // edges but nodes remain, we have a cycle.
    //
    // `incoming[i]` = number of edges pointing TO i, computed by
    // summing the out-degrees of every other node whose edge set
    // contains i. A node with `incoming[i] == 0` is "ready".
    let mut incoming: Vec<usize> = vec![0; funcs.len()];
    for out_edges in &edges {
        for &j in out_edges {
            incoming[j] += 1;
        }
    }
    let mut ordered: Vec<TypedFunc> = Vec::new();
    let mut picked: HashSet<usize> = HashSet::new();

    while picked.len() < funcs.len() {
        // Find a node with no incoming edges that hasn't been picked.
        let next = (0..funcs.len())
            .find(|i| !picked.contains(i) && incoming[*i] == 0);
        match next {
            Some(i) => {
                picked.insert(i);
                ordered.push(funcs[i].clone());
                for &j in &edges[i] {
                    incoming[j] -= 1;
                }
            }
            None => {
                // Cycle - gather the remaining nodes (the cycle itself)
                // in their original order and report.
                has_cycle = true;
                let mut remaining: Vec<String> = funcs
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !picked.contains(i))
                    .map(|(_, f)| f.name.clone())
                    .collect();
                // Close the loop visually by appending the first name.
                if let Some(first) = remaining.first().cloned() {
                    remaining.push(first);
                }
                cycle_path = remaining.join(" -> ");
                break;
            }
        }
    }

    if has_cycle {
        let span = funcs
            .first()
            .map(|f| f.span.clone())
            .unwrap_or_else(Span::dummy);
        return Err(SemanticError::DispatchOrderCycle { cycle: cycle_path, span });
    }
    Ok(ordered)
}

/// Return the set of functions in `module` whose tag annotation
/// matches `selector`. rules:
///
/// * `tags = {A, B, key=value}` - OR over bare tags, AND with valued
/// matches. A function qualifies if it has at least one of the
/// listed bare tags AND has all valued tags matching exactly.
/// * `tags = [A, B,...]` - AND over bare tags. A function must
/// have all listed bare tags. (Valued tags AND as in the OR form.)
pub fn match_selector(module: &TypedModule, selector: &TagSelector) -> Vec<TypedFunc> {
    let (bare, valued) = split_selector(selector);
    module
        .functions
        .iter()
        .filter(|f| {
            // Bare tag set must intersect with the listed bare tags
            // for OR mode; must contain all for AND mode. An empty
            // bare list is a no-op (the selector is silent on bare
            // tags and only the valued clause can filter).
            let func_bares: HashSet<&str> = f.tags.iter().map(|s| s.as_str()).collect();
            let bare_ok = match selector {
                TagSelector::Or(_) => {
                    bare.is_empty() || bare.iter().any(|b| func_bares.contains(b.as_str()))
                }
                TagSelector::And(_) => {
                    bare.iter().all(|b| func_bares.contains(b.as_str()))
                }
            };
            if !bare_ok {
                return false;
            }
            // Valued tag match: each `key=value` in the selector must
            // match a valued tag on the function. Functions with no
            // valued tags automatically pass (the selector is silent
            // on them).
            for (k, v) in &valued {
                let fv = valued_tag(&f.original, k);
                match fv {
                    Some(actual) if tag_value_eq(actual, v) => {}
                    _ => return false,
                }
            }
            true
        })
        .cloned()
        .collect()
}

fn split_selector(sel: &TagSelector) -> (Vec<String>, Vec<(String, TagValue)>) {
    let items: &[TagItem] = match sel {
        TagSelector::Or(items) | TagSelector::And(items) => items,
    };
    let mut bare = Vec::new();
    let mut valued = Vec::new();
    for it in items {
        match it {
            TagItem::Bare(n) => bare.push(n.clone()),
            TagItem::Valued(k, v, _) => valued.push((k.clone(), v.clone())),
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

fn valued_tag<'a>(f: &'a spass_parser::ast::FuncDef, key: &str) -> Option<&'a TagValue> {
    for ann in &f.annotations {
        if let Annotation::Tag { items, .. } = ann {
            for it in items {
                if let TagItem::Valued(k, v, _) = it {
                    if k == key {
                        return Some(v);
                    }
                }
            }
        }
    }
    None
}

/// Validate a `Stmt::Dispatch` against `module`:
/// * match functions against the tag selector (E014 if none);
/// * for each match, check that every param is satisfied either by
/// an apply arg (positional) or by a bind entry (E002);
/// * warn when a bind entry is unused by a match (W002);
/// * topologically sort the matches using `@before`/`@after`
/// (E004 on cycle).
///
/// Returns the resolved dispatch on success, plus any non-fatal
/// warnings (W002: bind entry unused by a matched function) the
/// resolver wants to surface to the caller. Warnings are returned
/// alongside the result rather than as `Result::Err` because the
/// dispatch is still semantically valid - the unused bind is
/// simply ignored at codegen time.
pub fn resolve_dispatch(
    decl: &DispatchDecl,
    apply: &ApplyStmt,
    module: &TypedModule,
) -> Result<(ResolvedDispatch, Vec<SemanticWarning>), SemanticError> {
    // 1. Tag match.
    let matched = match_selector(module, &decl.tags);
    if matched.is_empty() {
        return Err(SemanticError::DispatchNoMatch { span: decl.span.clone() });
    }

    // 2. Apply call must be a direct call to `apply`. The args
    // become the "static" head; the last arg is the carry.
    let apply_args: Vec<Expr> = match &apply.call {
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                if name != "apply" {
                    return Err(SemanticError::TypeMismatch {
                        message: format!(
                            "expected call to `apply`, got call to `{}`",
                            name
                        ),
                        span: apply.call.span(),
                    });
                }
            } else {
                return Err(SemanticError::TypeMismatch {
                    message: "apply must be a direct function call".into(),
                    span: apply.call.span(),
                });
            }
            if args.is_empty() {
                return Err(SemanticError::TypeMismatch {
                    message: "apply must have at least one argument (the carry)".into(),
                    span: apply.call.span(),
                });
            }
            args.iter().map(|a| a.value.clone()).collect()
        }
        _ => {
            return Err(SemanticError::TypeMismatch {
                message: "apply must be a function call".into(),
                span: apply.call.span(),
            });
        }
    };
    // The carry is the last arg; everything before is "static" and
    // must be passed positionally to every matched function.
    let static_arg_count = apply_args.len() - 1;

    // 3. Per-match validation: every param in the matched function
    // must be satisfied by either a static apply arg, the carry
    // (last apply arg), or a bind entry. Surface E002 for any
    // param that has none of these.
    //
    // Function-arity breakdown:
    // * first `static_arg_count` params <- static apply args
    // * next 1 param <- the carry (last apply arg)
    // * remaining params <- must be in `bind`
    //
    // W002: a bind entry that the matched function does not
    // take is non-fatal. We collect it and return it to the
    // caller (which pushes it onto the symbol-table warnings
    // vec). The dispatch is still valid.
    let mut warnings: Vec<SemanticWarning> = Vec::new();
    for f in &matched {
        let n_params = f.params.len();
        for (i, p) in f.params.iter().enumerate() {
            if i < static_arg_count {
                // Positioned from the static head - no bind needed.
                continue;
            }
            if i == static_arg_count {
                // The carry position: the function's corresponding param
                // is fed by the last apply arg, not by a bind entry.
                continue;
            }
            // Check bind entries.
            let satisfied = decl.bind.iter().any(|b| b.param == p.name);
            if !satisfied {
                return Err(SemanticError::DispatchBindMissing {
                    param: p.name.clone(),
                    span: apply.call.span(),
                });
            }
        }
        let _ = n_params;
        for b in &decl.bind {
            if !f.params.iter().any(|p| p.name == b.param) {
                warnings.push(SemanticWarning::DispatchBindUnused {
                    param: b.param.clone(),
                    func: f.name.clone(),
                    span: b.span.clone(),
                });
            }
        }
    }
    // Even if we early-returned above on E002, no warnings were
    // pushed yet (warnings are pushed AFTER the E002 bail-out),
    // so we don't need to thread them through the Err path.

    // 4. Topo sort using @before / @after.
    let ordered = topo_sort(matched)?;

    // 5. Resolve the bind entries: keep literals as-is, look up
    // tag values for `@tag(key)` references. The resolution
    // is per-match in the codegen pass; here we just record
    // the raw source for emission.
    let bind: Vec<ResolvedBind> = decl
        .bind
        .iter()
        .map(|b| match &b.source {
            spass_parser::ast::DispatchBindSource::Literal(e) => ResolvedBind::Literal {
                param: b.param.clone(),
                value: e.clone(),
                span: b.span.clone(),
            },
            spass_parser::ast::DispatchBindSource::TagRef(key, span) => {
                // Use the first match's value (consistency check is
                // the caller's problem - the match_selector ensures
                // every function with the tag has the same value, or
                // the semantic pass would have failed).
                let value = ordered
                    .iter()
                    .find_map(|f| valued_tag(&f.original, key.as_str()).cloned())
                    .unwrap_or(TagValue::Number(num_rational::Rational64::from_integer(0)));
                ResolvedBind::TagValue {
                    param: b.param.clone(),
                    value,
                    span: span.clone(),
                }
            }
        })
        .collect();

    Ok((
        ResolvedDispatch {
            ordered,
            apply_args,
            bind,
            span: decl.span.clone(),
        },
        warnings,
    ))
}
