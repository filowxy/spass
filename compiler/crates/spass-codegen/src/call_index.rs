//! Indexes call-site symbols and value domains per module.

use crate::{mangle_for_call, value_domain, ValueDomain};
use spass_semantic::TypedModule;
use std::collections::HashMap;

/// What a call site needs to know about its callee.
pub(crate) struct CallSite {
    /// The symbol the callee is invoked under.
    pub symbol: String,
    /// The value domain of each parameter, in declaration order.
    ///
    /// `None` for an `out` parameter - it is passed by pointer and
    /// carries the encoded slot, so the call site does not quantise it
    /// - and for an unconstrained type.
    pub param_domains: Vec<Option<ValueDomain>>,
    /// The value domain of the return type.
    pub return_domain: Option<ValueDomain>,
}

/// The module-wide index of call sites, keyed by function name.
///
/// A name that appears twice resolves to the last definition, which is
/// the same function the module's own name index resolves to.
pub(crate) struct CallIndex {
    sites: HashMap<String, CallSite>,
}

impl CallIndex {
    /// Build the index for a module.
    pub fn build(typed: &TypedModule) -> Self {
        let mut sites = HashMap::with_capacity(typed.functions.len());
        for f in &typed.functions {
            sites.insert(
                f.name.clone(),
                CallSite {
                    symbol: mangle_for_call(f),
                    param_domains: f
                        .params
                        .iter()
                        .map(|p| {
                            if p.is_out {
                                None
                            } else {
                                value_domain(&p.ty)
                            }
                        })
                        .collect(),
                    return_domain: f.return_type.as_ref().and_then(value_domain),
                },
            );
        }
        Self { sites }
    }

    /// The call-site data for a function, or `None` when the module has
    /// no function by that name.
    pub fn get(&self, name: &str) -> Option<&CallSite> {
        self.sites.get(name)
    }
}
