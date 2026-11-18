//! Applies optional compile-time optimisation passes, disabled by default.

pub mod const_eval;
pub mod reduce;

use std::collections::HashSet;

use spass_semantic::{SemanticError, TypedFunc, TypedModule};
use spass_transform::if_present::Variant;
use spass_transform::plan::{BodyPlan, FunctionPlan, ModulePlan};
use spass_transform::prepare::prepare_body;

use reduce::{Report, Switches};

/// The optimiser's configuration.
#[derive(Debug, Clone, Copy)]
pub struct OptConfig {
    /// The switches that decide which reductions run.
    pub switches: Switches,
    /// How deep a constant expression may be before the evaluator gives
    /// up. A deep expression would otherwise become a huge literal.
    pub max_fold_depth: usize,
}

impl Default for OptConfig {
    fn default() -> Self {
        OptConfig::disabled()
    }
}

impl OptConfig {
    /// The configuration with every reduction off - the default.
    pub fn disabled() -> OptConfig {
        OptConfig { switches: Switches::default(), max_fold_depth: 8 }
    }

    /// Whether any reduction is enabled.
    pub fn is_enabled(&self) -> bool {
        self.switches.readonly_locals
            || self.switches.inline_single_use
            || self.switches.fold_constants
            || self.switches.const_fold_arith
            || self.switches.dead_code_elim
    }

    /// The configuration as a stable list of flags, for the build cache
    /// key: two configurations that produce different artifacts must not
    /// share a cache entry.
    pub fn fingerprint(&self) -> Vec<String> {
        let mut out = Vec::new();
        let s = &self.switches;
        if s.readonly_locals {
            out.push("opt:readonly_locals".to_string());
        }
        if s.inline_single_use {
            out.push("opt:inline_single_use".to_string());
        }
        if s.fold_constants {
            out.push("opt:fold_constants".to_string());
        }
        if s.const_fold_arith {
            out.push("opt:const_fold_arith".to_string());
        }
        if s.dead_code_elim {
            out.push("opt:dead_code_elim".to_string());
        }
        out.push(format!("opt:max_fold_depth={}", self.max_fold_depth));
        out.sort();
        out
    }
}

/// The outcome of optimising a module.
#[derive(Debug, Clone, Default)]
pub struct OptReport {
    /// What the reductions did.
    pub report: Report,
    /// The functions that were not emitted because nothing referenced
    /// them.
    pub eliminated: Vec<String>,
}

/// The outcome of optimising one function.
#[derive(Debug, Clone, Default)]
pub struct FunctionReduction {
    /// The bodies to emit, one per `if_present` variant.
    pub variants: Vec<(Variant, BodyPlan)>,
    /// What the passes did.
    pub report: Report,
}

/// Optimise a whole module.
///
/// The plan is `None` when no reduction is enabled: the caller then
/// emits the module exactly as it did before the optimiser existed, and
/// the artifact is byte for byte the same.
pub fn optimise_module(
    module: &TypedModule,
    config: &OptConfig,
) -> Result<OptOutcome, SemanticError> {
    if !config.is_enabled() {
        return Ok(OptOutcome { plan: None, report: OptReport::default() });
    }

    // The functions nothing references are dropped when the switch is
    // on. A public function is never dropped: a host looks its symbol
    // up by name.
    let eliminated: Vec<String> = if config.switches.dead_code_elim {
        unreachable_functions(module)
    } else {
        Vec::new()
    };

    let mut report = Report::default();
    let mut plan: ModulePlan = ModulePlan::new();
    for func in &module.functions {
        let reduction = optimise_function(module, func, config)?;
        report.merge(reduction.report);
        let variants = if eliminated.iter().any(|n| n == &func.name) {
            Vec::new()
        } else {
            reduction.variants
        };
        plan.insert(func.name.clone(), FunctionPlan { variants });
    }

    Ok(OptOutcome {
        plan: Some(plan),
        report: OptReport { report, eliminated },
    })
}

/// The outcome of optimising a module.
#[derive(Debug, Clone, Default)]
pub struct OptOutcome {
    /// The plan the code generator emits from, or `None` when no
    /// reduction ran.
    pub plan: Option<ModulePlan>,
    /// What the passes did.
    pub report: OptReport,
}

/// Optimise one function.
///
/// A function with `if_present` parameters has two bodies - the present
/// variant and the absent variant - and each one is optimised on its
/// own: the two variants have different semantics, so their reductions
/// are allowed to differ.
pub fn optimise_function(
    module: &TypedModule,
    func: &TypedFunc,
    config: &OptConfig,
) -> Result<FunctionReduction, SemanticError> {
    let variants: Vec<Variant> = if func.has_if_present {
        vec![Variant::Present, Variant::Absent]
    } else {
        vec![Variant::Present]
    };

    // A suspending function keeps its locals in a state block. The moment
    // a `let` is evaluated is not the moment its value is used, so a
    // binding there is not a constant and the function is left alone.
    let suspended = func.has_await || func.has_checkpoint;

    let mut out = Vec::new();
    let mut report = Report::default();
    for variant in variants {
        // The compile-time constructs are resolved first: the reducer
        // has to see the statements that actually run. A `@dispatch`
        // assigns to its target, and a reducer that did not see the
        // assignment would fold the target away.
        let prepared = prepare_body(module, &func.original.body.stmts, variant)?;
        let label = format!("{}::{:?}", func.name, variant);
        let reduction = if suspended {
            reduce::Reduction {
                body: prepared,
                readonly: HashSet::new(),
                report: Report::default(),
            }
        } else {
            reduce::reduce_body(&prepared, config.switches, config.max_fold_depth, &label)
        };
        report.merge(reduction.report);
        out.push((
            variant,
            BodyPlan { stmts: reduction.body, readonly: reduction.readonly },
        ));
    }
    Ok(FunctionReduction { variants: out, report })
}

/// The functions nothing references.
///
/// A function is reachable when another function calls it, when a
/// sequence calls it, or when it is part of the public surface - a host
/// loads a module by symbol name and has no way to know which functions
/// the module calls internally.
///
/// `ponytail`: the reachability scan is lexical. A call through a
/// function value would be missed, but Spass has no first-class
/// function values, so the scan is exact for the language as it stands.
pub fn unreachable_functions(module: &TypedModule) -> Vec<String> {
    let mut called: std::collections::HashSet<String> = std::collections::HashSet::new();
    for func in &module.functions {
        collect_calls(&func.original.body.stmts, &mut called);
    }
    module
        .functions
        .iter()
        .filter(|f| {
            !called.contains(&f.name)
                && f.original.visibility != spass_parser::ast::Visibility::Public
                && !is_sequence(module, &f.name)
        })
        .map(|f| f.name.clone())
        .collect()
}

/// Whether a function is a sequence (sequences are always started).
fn is_sequence(module: &TypedModule, name: &str) -> bool {
    module.sequences.iter().any(|s| s.name == name)
}

/// Collect the names called from a statement list.
fn collect_calls(stmts: &[spass_parser::ast::Stmt], out: &mut std::collections::HashSet<String>) {
    use spass_parser::ast::Stmt;
    for stmt in stmts {
        match stmt {
            Stmt::Expr(e, _) => collect_calls_expr(e, out),
            Stmt::Let { value, .. } => collect_calls_expr(value, out),
            Stmt::Assign { value, .. } => collect_calls_expr(value, out),
            Stmt::Return { value: Some(v), .. } => collect_calls_expr(v, out),
            Stmt::If { cond, then_branch, else_branch, .. } => {
                collect_calls_expr(cond, out);
                collect_calls(&then_branch.stmts, out);
                collect_calls(&else_branch.stmts, out);
            }
            Stmt::For { body, .. } => collect_calls(&body.stmts, out),
            Stmt::While { cond, body, .. } => {
                collect_calls_expr(cond, out);
                collect_calls(&body.stmts, out);
            }
            _ => {}
        }
    }
}

fn collect_calls_expr(
    expr: &spass_parser::ast::Expr,
    out: &mut std::collections::HashSet<String>,
) {
    use spass_parser::ast::Expr;
    match expr {
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                out.insert(name.clone());
            }
            for a in args {
                collect_calls_expr(&a.value, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_calls_expr(lhs, out);
            collect_calls_expr(rhs, out);
        }
        Expr::Unary { expr, .. } => collect_calls_expr(expr, out),
        Expr::Index { target, index, .. } => {
            collect_calls_expr(target, out);
            collect_calls_expr(index, out);
        }
        Expr::FieldAccess { target, .. } => collect_calls_expr(target, out),
        Expr::Tuple { elements, .. } | Expr::ListLit { elements, .. } => {
            for e in elements {
                collect_calls_expr(e, out);
            }
        }
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                collect_calls_expr(v, out);
            }
        }
        Expr::Spawn { func, args, .. } | Expr::Parallel { func, args, .. } => {
            collect_calls_expr(func, out);
            for a in args {
                collect_calls_expr(&a.value, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spass_parser::Parser;

    fn typed(src: &str) -> TypedModule {
        let module = Parser::parse_source(src, "<test>").expect("parse");
        spass_semantic::analyze(&module).expect("analyse")
    }

    fn enabled() -> OptConfig {
        OptConfig {
            switches: Switches {
                readonly_locals: true,
                inline_single_use: true,
                fold_constants: true,
                const_fold_arith: true,
                dead_code_elim: false,
            },
            max_fold_depth: 8,
        }
    }

    /// With the optimiser off, there is no plan at all: the caller emits
    /// the module exactly as it did before.
    #[test]
    fn a_disabled_optimiser_produces_no_plan() {
        let module = typed("func f(a: float): float { let x: float = a + 1.0 return x }");
        let outcome = optimise_module(&module, &OptConfig::disabled()).expect("optimise");
        assert!(outcome.plan.is_none());
    }

    /// A function with `if_present` produces two bodies, and each one is
    /// reduced on its own.
    #[test]
    fn an_if_present_function_produces_two_bodies() {
        let src = r#"
            func f(a: float, b: float): float {
                if_present(a) { return a + 1.0 }
                return b
            }
        "#;
        let module = typed(src);
        let func = &module.functions[0];
        assert!(func.has_if_present);
        let reduction = optimise_function(&module, func, &enabled()).expect("optimise");
        assert_eq!(reduction.variants.len(), 2);
        let present = &reduction.variants[0].1.stmts;
        let absent = &reduction.variants[1].1.stmts;
        assert_ne!(present, absent);
    }

    /// A suspending function is left alone: its locals live in a state
    /// block and are not constants.
    #[test]
    fn an_awaiting_function_is_skipped() {
        let src = r#"
            func f(a: float): float {
                @await
                let v = spasio:input[file]("ch")
                return a
            }
        "#;
        let module = typed(src);
        let func = &module.functions[0];
        assert!(func.has_await);
        let reduction = optimise_function(&module, func, &enabled()).expect("optimise");
        assert!(reduction.variants[0].1.readonly.is_empty());
    }

    /// The configuration fingerprint distinguishes two configurations
    /// that produce different artifacts.
    #[test]
    fn the_fingerprint_distinguishes_configurations() {
        let off = OptConfig::disabled();
        let on = enabled();
        assert_ne!(off.fingerprint(), on.fingerprint());
        assert_eq!(off.fingerprint(), off.fingerprint());
    }

    /// A function nothing calls is unreachable - unless it is public,
    /// because a host looks public symbols up by name.
    #[test]
    fn an_unreferenced_private_function_is_unreachable() {
        let src = r#"
            fileclude func helper(x: float): float { return x }
            func main(a: float): float { return a }
        "#;
        let module = typed(src);
        let unreachable = unreachable_functions(&module);
        assert!(unreachable.contains(&"helper".to_string()));
        assert!(!unreachable.contains(&"main".to_string()));
    }

    /// The plan carries the reduced body, and the emitter is told which
    /// locals are read-only.
    #[test]
    fn the_plan_carries_the_reduced_body() {
        let src = r#"
            func f(a: float): float {
                let k: float = 2.0
                return a * k
            }
        "#;
        let module = typed(src);
        let outcome = optimise_module(&module, &enabled()).expect("optimise");
        let plan = outcome.plan.expect("a plan");
        let fp = plan.get("f").expect("the function");
        let body = &fp.variants[0].1.stmts;
        // The constant binding is gone and its value stands at the use
        // site.
        assert!(!body
            .iter()
            .any(|s| matches!(s, spass_parser::ast::Stmt::Let { names, .. } if names == &["k".to_string()])));
    }
}
