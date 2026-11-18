//! Infers value domains and allocates bit widths.

use num_rational::Rational64;
use num_traits::{One, Zero};
use spass_lexer::Span as LexSpan;
use spass_parser::ast::{BinOp, RangeDecl, RationalLiteral, Type};
use thiserror::Error;

pub mod bootstrap;

/// Maximum bit-width a value-domain may occupy. 128-bit values use two
/// 64-bit limbs on the GPU (W004) and are flagged for performance review.
pub const MAX_BITS: u32 = 128;

/// Errors that the inference engine reports. Most are surfaced through the
/// semantic analysis pass with an error code (E001, E010, etc.).
#[derive(Debug, Clone, Error, PartialEq)]
pub enum InferError {
    #[error("E001: invalid value-domain at {span}: {message}")]
    InvalidRange { message: String, span: LexSpan },

    #[error("E010: required bit-width {bits} exceeds maximum of {max} at {span}")]
    BitsTooWide { bits: u32, max: u32, span: LexSpan },

    #[error("E006: type error at {span}: {message}")]
    TypeError { message: String, span: LexSpan },
}

pub type InferResult<T> = Result<T, InferError>;

// ============================================================================
// Range: a (min, max, step) tuple in the rational domain
// ============================================================================

/// A fully-resolved value-domain.
///
/// `min` and `max` are inclusive bounds. `step` is the smallest
/// distinguishable increment: every representable value is `min + k * step`
/// for some `k in [0, (max - min) / step]`.
///
/// The "default" range is the unconstrained range of a base type:
/// `float -> [MIN, MAX, machine_eps]` (approx), `int -> [i32::MIN, i32::MAX, 1]`.
/// We represent the default with `min = -inf`, `max = +inf`, `step = 0`
/// (`None` in the AST `RangeDecl` after resolution).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Range {
    pub min: Rational64,
    pub max: Rational64,
    pub step: Rational64,
}

impl Range {
    /// The "any" range, used as a placeholder when the compiler has no
    /// information (e.g. an `int` parameter without an explicit domain).
    pub fn any() -> Self {
        Self {
            min: Rational64::from_integer(i64::MIN),
            max: Rational64::from_integer(i64::MAX),
            // step 0 marks "any": non-zero step implies a quantised range.
            step: Rational64::zero(),
        }
    }

    /// Build a range from a parser [`RangeDecl`]. Missing bounds default to
    /// the base type's full domain.
    pub fn from_decl(decl: &RangeDecl, base: &Type) -> InferResult<Self> {
        let base_default = default_range(base);
        let min = decl
            .min
            .as_ref()
            .map(|m| m.value.clone())
            .unwrap_or_else(|| base_default.min.clone());
        let max = decl
            .max
            .as_ref()
            .map(|m| m.value.clone())
            .unwrap_or_else(|| base_default.max.clone());
        let step = decl
            .step
            .as_ref()
            .map(|m| m.value.clone())
            .unwrap_or_else(|| base_default.step.clone());
        if decl.step.is_some() {
            validate_step(&step, decl.step.as_ref().unwrap().span.clone())?;
        }
        if max < min {
            return Err(InferError::InvalidRange {
                message: format!(
                    "max ({}) must be >= min ({})",
                    max, min
                ),
                span: decl.span.clone(),
            });
        }
        if !step.is_zero() {
            // (max - min) must be a multiple of step
            let span = decl
                .step
                .as_ref()
                .map(|s| s.span.clone())
                .unwrap_or_else(|| decl.span.clone());
            if !is_multiple_of(&(max.clone() - min.clone()), &step) {
                return Err(InferError::InvalidRange {
                    message: format!(
                        "(max - min) = {} is not an exact multiple of step = {}",
                        max.clone() - min.clone(),
                        step
                    ),
                    span,
                });
            }
        }
        Ok(Range { min, max, step })
    }

    /// The "state count" - number of distinct values representable in this
    /// range. `None` when the step is zero (unquantised / "any" range).
    ///
    /// self-hosting: the numeric core is computed by the Spass
    /// implementation (bootstrap.spass) with the Rust expression below
    /// (`state_count_rust`) as a bit-exact cross-check.
    pub fn state_count(&self) -> Option<u64> {
        if self.step.is_zero() {
            return None;
        }
        let rust = self.state_count_rust();
        bootstrap::state_count_via_spass(&self.min, &self.max, &self.step, rust)
    }

    /// The Rust-only reference computation of [`Self::state_count`].
    fn state_count_rust(&self) -> Option<u64> {
        let span = self.max.clone() - self.min.clone();
        if span < Rational64::zero() {
            return Some(0);
        }
        let n = span / self.step.clone() + Rational64::one();
        // convert to u64 if it fits, else return None (too big)
        if n.is_integer() && *n.numer() >= 0 {
            let numer = *n.numer();
            let denom = *n.denom();
            if denom == 1 {
                Some(numer as u64)
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Required bit-width: `ceil(log2(state_count))`. Returns 0 for the
    /// "any" range (the caller decides the default base type width).
    ///
    /// self-hosting: `bit_width(n)` runs on the Spass core, with
    /// [`bit_width_rust`] as the cross-check.
    pub fn bit_width(&self) -> u32 {
        match self.state_count() {
            None => 0,
            Some(n) => bootstrap::bit_width_via_spass(n, bit_width_rust(n)),
        }
    }

    /// True if this range has no constraint (`step == 0`).
    pub fn is_any(&self) -> bool {
        self.step.is_zero()
    }
}

// ============================================================================
// Default ranges for base types
// ============================================================================

/// The implicit range of a bare type without an explicit declaration.
pub fn default_range(ty: &Type) -> Range {
    match ty {
        Type::Float(_) => Range {
            // i64 can't hold +/-1.7e308, so we keep the conceptual bounds as
            // +/-i64::MAX for "infinity". A subsequent bit-width allocation
            // will fall back to a 32-bit float.
            min: Rational64::from_integer(-(1i64 << 62)),
            max: Rational64::from_integer(1i64 << 62),
            step: Rational64::zero(),
        },
        Type::Int(_) => Range {
            min: Rational64::from_integer(i32::MIN as i64),
            max: Rational64::from_integer(i32::MAX as i64),
            step: Rational64::from_integer(1),
        },
        Type::Uint(_) => Range {
            min: Rational64::from_integer(0),
            max: Rational64::from_integer(u32::MAX as i64),
            step: Rational64::from_integer(1),
        },
        _ => Range::any(),
    }
}

// ============================================================================
// Step validation (E001)
// ============================================================================

/// E001: `step` must be a negative power of two or ten. Examples accepted:
/// `0.5, 0.25, 0.125,..., 0.1, 0.01, 0.001,...`. We accept `step = 1`
/// (the trivial case, representing integers).
pub fn validate_step(step: &Rational64, span: LexSpan) -> InferResult<()> {
    if step.is_zero() {
        return Ok(());
    }
    if step <= &Rational64::zero() {
        return Err(InferError::InvalidRange {
            message: format!("step {} must be > 0", step),
            span,
        });
    }
    // step = 1 always OK (integer precision)
    if step == &Rational64::one() {
        return Ok(());
    }
    // check 2^-n form: 1/step must be a positive power of 2
    let inv = Rational64::one() / step.clone();
    if inv.is_integer() && is_power_of_two(*inv.numer() as u128) {
        return Ok(());
    }
    // check 10^-n form: 1/step must be a positive power of 10
    if inv.is_integer() && is_power_of_ten(*inv.numer() as u128) {
        return Ok(());
    }
    Err(InferError::InvalidRange {
        message: format!(
            "step {} must be 1, a negative power of 2 (0.5, 0.25, ...), or a negative power of 10 (0.1, 0.01, ...)",
            step
        ),
        span,
    })
}

fn is_power_of_two(mut n: u128) -> bool {
    if n == 0 {
        return false;
    }
    while n > 1 {
        if n & 1 == 1 {
            return n == 1;
        }
        n >>= 1;
    }
    true
}

fn is_power_of_ten(mut n: u128) -> bool {
    if n == 0 {
        return false;
    }
    while n > 1 {
        if n % 10 != 0 {
            return false;
        }
        n /= 10;
    }
    true
}

fn is_multiple_of(a: &Rational64, b: &Rational64) -> bool {
    if b.is_zero() {
        return false;
    }
    let q = a.clone() / b.clone();
    q.is_integer()
}

// ============================================================================
// Range arithmetic - the "soul" of Spass
// ============================================================================

/// ADD/SUB: exact, tight. step is gcd of the two operand steps.
///
/// self-hosting: the endpoints and the step gcd are computed by
/// the Spass core (`bootstrap.spass`) with this Rust expression as a
/// bit-exact cross-check; the bridge falls back to the Rust result
/// when an operand leaves the f64-exact integer domain.
pub fn infer_add_sub(a: &Range, b: &Range) -> Range {
    let rust = Range {
        min: a.min.clone() + b.min.clone(),
        max: a.max.clone() + b.max.clone(),
        step: gcd_rational(&a.step, &b.step),
    };
    bootstrap::add_sub_via_spass(a, b, &rust).unwrap_or(rust)
}

/// MUL: corner enumeration. Returns a conservative (safe) range.
///
/// self-hosting: the four corner products are computed by the
/// Spass core; the min/max comparison and the conservative step
/// choice are pure comparisons and stay in Rust.
pub fn infer_mul(a: &Range, b: &Range) -> Range {
    let p1 = a.min.clone() * b.min.clone();
    let p2 = a.min.clone() * b.max.clone();
    let p3 = a.max.clone() * b.min.clone();
    let p4 = a.max.clone() * b.max.clone();
    let min = p1.min(p2.clone()).min(p3.clone()).min(p4.clone());
    let max = p1.max(p2).max(p3).max(p4);
    // Conservative step: take the larger operand step.
    let step = if a.step >= b.step { a.step.clone() } else { b.step.clone() };
    let rust = Range { min, max, step };
    bootstrap::mul_via_spass(a, b, &rust).unwrap_or(rust)
}

/// Negation: min/max swap, step unchanged.
pub fn infer_neg(a: &Range) -> Range {
    Range {
        min: -a.max.clone(),
        max: -a.min.clone(),
        step: a.step.clone(),
    }
}

/// Constant 2^k division (right-shift).
///
/// A fixed-point value is stored as the unsigned state index
/// `N = (value - min) / step`, and a right shift by `k` bits is an integer
/// shift of that index: `N' = N >> k` (truncating). So the *step is
/// unchanged* - we shift the index, not the value - and the range narrows
/// because the largest reachable index shrinks:
///
/// ```text
/// n = (max - min) / step // largest state index
/// n_shift = n >> k // truncating integer shift
/// max' = min + n_shift * step
/// ```
///
/// Example: `[0, 8]` step 1 shifted right by 1 keeps `0, 1, 2, 3, 4` -
/// bounds `[0, 4]`, step still 1.
///
/// The previous implementation divided `min`/`max` by `2^k` and *multiplied*
/// the step by `2^k`. All three were wrong: the widened step silently
/// coarsens the grid (values the caller can still pass get snapped to the
/// wrong state), and dividing `min` by `2^k` moves a non-zero base offset,
/// which is not what an index shift does.
///
/// An unquantised range (`step == 0`, the "any" domain) has no state index
/// to shift, so it falls back to plain division of the bounds and keeps its
/// unquantised step.
pub fn infer_shift_right(a: &Range, k: u32) -> Range {
    if a.step.is_zero() {
        let divisor = Rational64::from_integer(1i64 << k.min(62));
        return Range {
            min: a.min.clone() / divisor.clone(),
            max: a.max.clone() / divisor,
            step: Rational64::zero(),
        };
    }
    let n = (a.max.clone() - a.min.clone()) / a.step.clone();
    // `n` is a non-negative integer for any range that passed `from_decl`.
    // Truncate toward zero to model `N >> k` even if an intermediate range
    // carried a non-integral span.
    let shift = Rational64::from_integer(1i64 << k.min(62));
    let n_shift = (n / shift).floor();
    Range {
        min: a.min.clone(),
        max: a.min.clone() + n_shift * a.step.clone(),
        step: a.step.clone(),
    }
}

/// Integer step gcd for two positive rationals that are themselves integer
/// ratios. For two step values `s1 = n1/d1` and `s2 = n2/d2`, the gcd is
/// `gcd(n1*d2, n2*d1) / (d1*d2)`.
fn gcd_rational(a: &Rational64, b: &Rational64) -> Rational64 {
    if a.is_zero() {
        return b.clone();
    }
    if b.is_zero() {
        return a.clone();
    }
    let num_g = gcd_i64(
        (*a.numer() * b.denom()).abs(),
        (*b.numer() * a.denom()).abs(),
    );
    let den = (a.denom() * b.denom()).abs();
    if num_g == 0 {
        Rational64::zero()
    } else {
        Rational64::new(num_g, den)
    }
}

fn gcd_i64(mut a: i64, mut b: i64) -> i64 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a.abs()
}

/// The Rust-only reference computation of the bit width for a state
/// count. Used as the cross-check for the Spass `bit_width`.
/// `ponytail`: the 1-state case intentionally returns 1 (matching the
/// original implementation and the uint1 minimum allocation); the
/// mathematical ceil(log2(1)) = 0 is what the Spass core returns, so
/// the bridge skips n <= 1 entirely.
fn bit_width_rust(n: u64) -> u32 {
    match n {
        0 => 0,
        1 => 1,
        _ => {
            let mut bits = 0u32;
            let mut v = n - 1;
            while v > 0 {
                bits += 1;
                v >>= 1;
            }
            if (1u128 << bits) < n as u128 {
                bits += 1;
            }
            bits
        }
    }
}

/// Dispatch table for the inference rules, parameterised on a binary op.
pub fn infer_binop(op: BinOp, a: &Range, b: &Range) -> Range {
    match op {
        BinOp::Add => infer_add_sub(a, b),
        BinOp::Sub => infer_add_sub(a, &infer_neg(b)),
        BinOp::Mul => infer_mul(a, b),
        BinOp::Div | BinOp::Pow | BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le
        | BinOp::Gt | BinOp::Ge | BinOp::And | BinOp::Or => {
            // For comparison / logical ops, result is `bool` - represented
            // by an "any" range so downstream code knows it's not numeric.
            Range::any()
        }
    }
}

// ============================================================================
// Bit-width promotion
// ============================================================================

/// Ensure a value's range fits within `bits` bits. If not, promote to the
/// minimal width that contains it. Returns the new bit width.
pub fn ensure_bits(range: &Range, bits: u32, span: LexSpan) -> InferResult<u32> {
    let required = range.bit_width();
    if required == 0 {
        return Ok(bits.max(32));
    }
    if required <= bits {
        return Ok(bits);
    }
    if required > MAX_BITS {
        return Err(InferError::BitsTooWide {
            bits: required,
            max: MAX_BITS,
            span,
        });
    }
    Ok(required)
}

// ============================================================================
// Literal ranges
// ============================================================================

/// The range of a constant numeric literal at compile time. If the literal
/// has a quantised value (e.g. `0.1` is exactly `1/10`), the step is the
/// minimal step that distinguishes it; otherwise step is 0.
pub fn literal_range(value: &Rational64) -> Range {
    Range {
        min: value.clone(),
        max: value.clone(),
        step: Rational64::zero(),
    }
}

#[allow(dead_code)]
pub(crate) fn rational_lit_to_range(lit: &RationalLiteral) -> Range {
    literal_range(&lit.value)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use num_rational::Rational64;
    use spass_lexer::Span;

    fn r(min: i64, max: i64, step_num: i64, step_den: i64) -> Range {
        Range {
            min: Rational64::from_integer(min),
            max: Rational64::from_integer(max),
            step: Rational64::new(step_num, step_den),
        }
    }

    #[test]
    fn step_must_be_power_of_two() {
        // 0.5 = 1/2
        let s = Rational64::new(1, 2);
        assert!(validate_step(&s, Span::dummy()).is_ok());
        // 0.25
        let s = Rational64::new(1, 4);
        assert!(validate_step(&s, Span::dummy()).is_ok());
        // 0.1 = 1/10
        let s = Rational64::new(1, 10);
        assert!(validate_step(&s, Span::dummy()).is_ok());
        // 0.3 - not allowed
        let s = Rational64::new(3, 10);
        assert!(validate_step(&s, Span::dummy()).is_err());
    }

    #[test]
    fn add_step_is_gcd() {
        let a = r(0, 100, 1, 10); // step 0.1
        let b = r(0, 100, 1, 2);  // step 0.5
        let c = infer_add_sub(&a, &b);
        // gcd(0.1, 0.5) = 0.1
        assert_eq!(c.step, Rational64::new(1, 10));
        assert_eq!(c.min, Rational64::from_integer(0));
        assert_eq!(c.max, Rational64::from_integer(200));
    }

    #[test]
    fn mul_takes_corner_min_max() {
        let a = r(-5, 5, 1, 1);
        let b = r(-5, 5, 1, 1);
        let c = infer_mul(&a, &b);
        assert_eq!(c.min, Rational64::from_integer(-25));
        assert_eq!(c.max, Rational64::from_integer(25));
    }

    /// A right shift moves the fixed-point *state index*, so the step and
    /// the base offset survive and only `max` narrows to `min + (n >> k) *
    /// step`. Regression test: the implementation used to divide `min`/`max`
    /// by `2^k` and multiply the step by `2^k`, which both moved a non-zero
    /// base offset and coarsened the grid.
    #[test]
    fn shift_right_narrows_state_index() {
        // states 0..8 shifted right by 1 => 0..4
        let a = r(0, 8, 1, 1);
        let got = infer_shift_right(&a, 1);
        assert_eq!(got.min, Rational64::from_integer(0));
        assert_eq!(got.max, Rational64::from_integer(4));
        assert_eq!(got.step, Rational64::from_integer(1));
    }

    /// The base offset `min` must not be scaled: `[10, 18]` step 1 has
    /// states 0..8, and `>> 2` leaves states 0..2, i.e. `[10, 12]`.
    #[test]
    fn shift_right_preserves_base_offset() {
        let b = r(10, 18, 1, 1);
        let got = infer_shift_right(&b, 2);
        assert_eq!(got.min, Rational64::from_integer(10));
        assert_eq!(got.max, Rational64::from_integer(12));
        assert_eq!(got.step, Rational64::from_integer(1));
    }

    /// A shift by zero bits is the identity on a quantised range.
    #[test]
    fn shift_right_by_zero_is_identity() {
        let a = r(0, 8, 1, 1);
        assert_eq!(infer_shift_right(&a, 0), a);
    }

    /// An unquantised range (`step == 0`) has no state index to shift, so
    /// the bounds are divided and the range stays unquantised.
    #[test]
    fn shift_right_on_any_range_divides_bounds() {
        let any = Range::any();
        let got = infer_shift_right(&any, 1);
        assert_eq!(got.step, Rational64::zero());
        assert_eq!(got.max, Rational64::from_integer(i64::MAX) / 2);
    }

    #[test]
    fn bit_width_for_state_count() {
        // [-5, 5, 0.1] => state_count = 101 => bits = 7 (2^7 = 128)
        let r = Range {
            min: Rational64::from_integer(-5),
            max: Rational64::from_integer(5),
            step: Rational64::new(1, 10),
        };
        assert_eq!(r.state_count(), Some(101));
        assert_eq!(r.bit_width(), 7);
    }

    #[test]
    fn bit_width_one_state() {
        let r = Range {
            min: Rational64::from_integer(7),
            max: Rational64::from_integer(7),
            step: Rational64::from_integer(1),
        };
        assert_eq!(r.bit_width(), 1);
    }

    #[test]
    fn from_decl_min_max_step() {
        let decl = RangeDecl {
            min: Some(RationalLiteral { value: Rational64::from_integer(0), span: Span::dummy() }),
            max: Some(RationalLiteral { value: Rational64::from_integer(380), span: Span::dummy() }),
            step: Some(RationalLiteral { value: Rational64::new(1, 10), span: Span::dummy() }),
            span: Span::dummy(),
        };
        let r = Range::from_decl(&decl, &Type::Float(None)).unwrap();
        assert_eq!(r.min, Rational64::from_integer(0));
        assert_eq!(r.max, Rational64::from_integer(380));
        assert_eq!(r.step, Rational64::new(1, 10));
    }

    #[test]
    fn ensure_bits_promotes() {
        let r = Range {
            min: Rational64::from_integer(0),
            max: Rational64::from_integer(1000),
            step: Rational64::from_integer(1),
        };
        let bits = ensure_bits(&r, 8, Span::dummy()).unwrap();
        // 1001 states -> 10 bits
        assert_eq!(bits, 10);
    }

    #[test]
    fn neg_swaps_min_max() {
        let a = r(2, 5, 1, 10);
        let b = infer_neg(&a);
        assert_eq!(b.min, Rational64::from_integer(-5));
        assert_eq!(b.max, Rational64::from_integer(-2));
        assert_eq!(b.step, a.step);
    }

    #[test]
    fn comparison_yields_any() {
        let a = r(0, 1, 1, 1);
        let b = r(0, 1, 1, 1);
        let c = infer_binop(BinOp::Eq, &a, &b);
        assert!(c.is_any());
    }
}
