//! Describes the exported ABI and manifest of a compiled module.

use num_traits::ToPrimitive;
use spass_parser::ast::Type;
use spass_semantic::TypedModule;

/// The value-domain encoding of one scalar.
#[derive(Clone, Debug)]
pub struct DomainAbi {
    /// Lower bound of the domain.
    pub min: f64,
    /// Upper bound of the domain.
    pub max: f64,
    /// Quantisation step.
    pub step: f64,
    /// The exact reciprocal of `step` (`k = 1/step`). The encoder
    /// multiplies by this instead of dividing by `step`, so the
    /// quantisation is exact rather than a rounded division.
    pub k: f64,
    /// How many distinct states the domain has.
    pub state_count: u64,
    /// The width of the state index in bits.
    pub bits: u32,
    /// The C type at the boundary (`uint8_t`, `uint16_t`,...).
    pub c_type: String,
}

/// How one parameter crosses the ABI boundary.
#[derive(Clone, Debug)]
pub struct ParamAbi {
    /// The Spass-level parameter name.
    pub name: String,
    /// The C type at the boundary (`double`, `int32_t`, `Point`,...).
    pub c_type: String,
    /// True for `out` parameters: the value is passed by
    /// pointer and is never quantised.
    pub is_out: bool,
    /// The value domain, when the parameter has one. An in-parameter
    /// with a domain crosses the boundary as the encoded state index,
    /// so the host must encode before the call and decode after it.
    pub domain: Option<DomainAbi>,
}

/// How the return value crosses the ABI boundary.
#[derive(Clone, Debug)]
pub struct ReturnAbi {
    /// The C type at the boundary. `void` when the function returns
    /// nothing.
    pub c_type: String,
    /// The value domain of the return value, when it has one. The
    /// function returns the encoded state index, so the host decodes it.
    pub domain: Option<DomainAbi>,
}

/// One exported function.
#[derive(Clone, Debug)]
pub struct ExportAbi {
    /// The Spass-level name (`height`).
    pub name: String,
    /// The exported symbol (`_spass_height_2_f_f`).
    pub symbol: String,
    /// The parameters, in declaration order.
    pub params: Vec<ParamAbi>,
    /// The return value, or `None` for a function that returns nothing.
    pub returns: Option<ReturnAbi>,
}

/// The layout of one struct across the ABI boundary.
///
/// Every field is an `f64` and the struct is `#[repr(C)]`, so the layout
/// is the field list in declaration order and nothing else.
#[derive(Clone, Debug)]
pub struct StructAbi {
    pub name: String,
    pub fields: Vec<String>,
}

/// Describe every function the module exports.
///
/// A function with `if_present` parameters has two
/// exported variants, and both are listed: the host picks the variant
/// whose behaviour it wants.
pub fn export_abis(typed: &TypedModule) -> Vec<ExportAbi> {
    let mut out = Vec::new();
    for func in &typed.functions {
        if !super::is_public_export(func) {
            continue;
        }
        let params: Vec<ParamAbi> = func
            .params
            .iter()
            .map(|p| ParamAbi {
                name: p.name.clone(),
                c_type: super::c_type_name(&p.ty),
                is_out: p.is_out,
                domain: domain_abi(&p.ty),
            })
            .collect();
        let returns = match &func.return_type {
            Some(ty) => Some(ReturnAbi {
                c_type: super::c_type_name(ty),
                domain: domain_abi(ty),
            }),
            // a multi-return function hands back a struct
            // of the element types.
            None if func.multi_returns.is_some() => Some(ReturnAbi {
                c_type: format!("SpassRet{}", func.multi_returns.as_ref().unwrap().len()),
                domain: None,
            }),
            None => None,
        };
        if func.has_if_present {
            for variant in [
                super::if_present_codegen::Variant::Present,
                super::if_present_codegen::Variant::Absent,
            ] {
                out.push(ExportAbi {
                    name: func.name.clone(),
                    symbol: super::mangle_function_name_variant(func, variant),
                    params: params.clone(),
                    returns: returns.clone(),
                });
            }
        } else {
            out.push(ExportAbi {
                name: func.name.clone(),
                symbol: super::mangle_for_call(func),
                params,
                returns,
            });
        }
    }
    out
}

/// Describe every struct the module defines.
pub fn struct_abis(typed: &TypedModule) -> Vec<StructAbi> {
    typed
        .structs
        .iter()
        .map(|s| StructAbi {
            name: s.name.clone(),
            fields: s.fields.iter().map(|(name, _)| name.clone()).collect(),
        })
        .collect()
}

/// The domain description of a type, if it has one.
fn domain_abi(ty: &Type) -> Option<DomainAbi> {
    let vd = super::value_domain(ty)?;
    Some(DomainAbi {
        min: rational_to_f64(&vd.min_r),
        max: rational_to_f64(&vd.max_r),
        step: rational_to_f64(&vd.step_r),
        k: vd.k.parse::<f64>().ok()?,
        state_count: vd.state_count,
        bits: vd.bits,
        c_type: vd.c_type.clone(),
    })
}

fn rational_to_f64(r: &num_rational::Rational64) -> f64 {
    r.to_f64().unwrap_or(f64::NAN)
}
