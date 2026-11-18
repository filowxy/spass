//! Defines the emission plan passed from the optimiser to the code generator.

use std::collections::{HashMap, HashSet};

use spass_parser::ast::Stmt;

use crate::if_present::Variant;

/// The body the generator emits for one function variant.
#[derive(Debug, Clone, Default)]
pub struct BodyPlan {
    /// The statements to emit.
    pub stmts: Vec<Stmt>,
    /// The locals that are never reassigned anywhere in the body.
    ///
    /// A name is in the set only when *no* binding of that name in the
    /// body is ever assigned to: the generator decides `let` versus
    /// `let mut` per name, so a name that is assigned in one scope and
    /// read-only in another has to stay mutable in both.
    pub readonly: HashSet<String>,
}

/// What the generator is told about one function.
#[derive(Debug, Clone, Default)]
pub struct FunctionPlan {
    /// The bodies to emit, one per `if_present` variant, in the order
    /// the generator emits them.
    ///
    /// An empty list means the function is not emitted at all - that is
    /// what the dead-code elimination pass produces.
    pub variants: Vec<(Variant, BodyPlan)>,
}

impl FunctionPlan {
    /// The plan for a function that is emitted from a single body.
    pub fn single(body: BodyPlan) -> FunctionPlan {
        FunctionPlan { variants: vec![(Variant::Present, body)] }
    }

    /// The body of one variant, if the plan has one.
    pub fn body(&self, variant: Variant) -> Option<&BodyPlan> {
        self.variants.iter().find(|(v, _)| *v == variant).map(|(_, b)| b)
    }
}

/// The plans of a module, keyed by function name.
///
/// A function that is not in the map is emitted from its source body.
pub type ModulePlan = HashMap<String, FunctionPlan>;
