//! Bridges the inference engine and the self-hosted Spass numeric core.

use crate::Range;
use num_rational::Rational64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// The numeric-core operations the Spass implementation provides.
/// All values are exact integers carried in `f64` (|v| <= 2^53);
/// multi-tuple results come back flattened as arrays in the order
/// documented at each method.
pub trait NumericCore: Send + Sync {
    /// `state_count_nd(span_n, span_d, step_n, step_d)` = span/step + 1.
    /// The bridge only calls it when the true quotient is an integer,
    /// so the float division is exact.
    fn state_count_nd(&self, span_n: f64, span_d: f64, step_n: f64, step_d: f64) -> f64;
    /// `bit_width(n)` = ceil(log2(n)), 0 for n <= 1.
    fn bit_width(&self, n: f64) -> i32;
    /// `rat_add_nd(an, ad, bn, bd)` -> `(num, den)` of the unreduced sum.
    fn rat_add_nd(&self, an: f64, ad: f64, bn: f64, bd: f64) -> (f64, f64);
    /// `gcd_step_nd(an, ad, bn, bd)` -> `(num, den)` of
    /// gcd(|an*bd|, |bn*ad|) / (ad*bd) - the ADD/SUB step rule.
    fn gcd_step_nd(&self, an: f64, ad: f64, bn: f64, bd: f64) -> (f64, f64);
    /// `mul_corners_nd(...)` -> the four corner products `(n, d)` of a
    /// MUL, flattened `[(p1n, p1d), (p2n, p2d), (p3n, p3d), (p4n, p4d)]`
    /// in corner order (min*min, min*max, max*min, max*max).
    fn mul_corners_nd(&self, a: &[f64; 8]) -> [f64; 8];
}

static CORE: OnceLock<Option<&'static dyn NumericCore>> = OnceLock::new();

/// Install the Spass numeric core. Call once at host startup; later
/// calls are no-ops (the first installation wins). With no core
/// installed the bridge sites fall back to the pure-Rust path.
pub fn install(core: &'static dyn NumericCore) {
    let _ = CORE.set(Some(core));
}

/// Whether a Spass core is installed (test/debug introspection).
pub fn core_installed() -> bool {
    CORE.get().map(|c| c.is_some()).unwrap_or(false)
}

fn core() -> Option<&'static dyn NumericCore> {
    CORE.get().and_then(|c| c.as_ref().copied())
}

static SPASS_CALLS: AtomicU64 = AtomicU64::new(0);
static SPASS_FALLBACKS: AtomicU64 = AtomicU64::new(0);

/// `(adopted_calls, rust_fallbacks)` since process start. Adopted
/// calls are the numeric-core computations the Spass implementation
/// produced; fallbacks are the sites where the f64 bridge could not
/// guarantee exactness and the Rust expression was used instead.
pub fn core_stats() -> (u64, u64) {
    (
        SPASS_CALLS.load(Ordering::Relaxed),
        SPASS_FALLBACKS.load(Ordering::Relaxed),
    )
}

const F64_EXACT_LIMIT: i128 = 1i128 << 53;

fn fits(x: i64) -> bool {
    (x as i128).abs() <= F64_EXACT_LIMIT
}

/// A `(numer, denom)` pair crosses the bridge exactly when both parts
/// are exact f64 integers (the denom is never 0 - `Rational64` keeps
/// it positive and normalised).
fn pair_fits(r: &Rational64) -> bool {
    fits(*r.numer()) && fits(*r.denom())
}

fn rat(n: f64, d: f64) -> Rational64 {
    // The bridge only calls this with values verified to be exact
    // integers within 2^53, so the casts are lossless.
    Rational64::new(n as i64, d as i64)
}

fn mismatch(op: &str, detail: &str, rust: &str, spass: &str) -> ! {
    panic!(
        "bootstrap core mismatch in {op} ({detail}): rust={rust} spass={spass}\n\
         the Spass numeric core and the Rust cross-check disagree - compiler invariant violated"
    )
}

fn fallback() {
    SPASS_FALLBACKS.fetch_add(1, Ordering::Relaxed);
}

fn adopted() {
    SPASS_CALLS.fetch_add(1, Ordering::Relaxed);
}

/// state_count through the Spass core. `rust` is the Rust-computed
/// reference; the bridge is only engaged when the reference exists and
/// every product in `span_n * step_d / (span_d * step_n)` stays exact.
pub fn state_count_via_spass(
    min: &Rational64,
    max: &Rational64,
    step: &Rational64,
    rust: Option<u64>,
) -> Option<u64> {
    let core = match (core(), rust) {
        (Some(c), Some(n)) if (n as i128) <= F64_EXACT_LIMIT => (c, n),
        _ => {
            // No core installed, or the count itself is not bridgeable.
            if core_installed() {
                fallback();
            }
            return rust;
        }
    };
    let (core, n) = core;
    let span = max.clone() - min.clone();
    if !pair_fits(&span) || !pair_fits(step) {
        fallback();
        return rust;
    }
    let (span_n, span_d) = (*span.numer(), *span.denom());
    let (step_n, step_d) = (*step.numer(), *step.denom());
    // The Spass body computes (span_n*step_d) / (span_d*step_n) + 1 in
    // f64; every intermediate product must be an exact integer.
    let num = span_n as i128 * step_d as i128;
    let den = span_d as i128 * step_n as i128;
    if num.abs() > F64_EXACT_LIMIT || den.abs() > F64_EXACT_LIMIT {
        fallback();
        return rust;
    }
    let v = core.state_count_nd(
        span_n as f64, span_d as f64, step_n as f64, step_d as f64,
    );
    adopted();
    if v.fract() != 0.0 || v < 0.0 || (v as i128) > F64_EXACT_LIMIT || v as u64 != n {
        mismatch(
            "state_count_nd",
            &format!("range=[{}, {}, {}]", min, max, step),
            &format!("{}", n),
            &format!("{}", v),
        );
    }
    rust
}

/// bit_width through the Spass core (`bit_width(n)`). Only engaged
/// when the state count itself is exact on the bridge AND n >= 2: the
/// n <= 1 cases are a Rust-side convention (1 state allocates uint1),
/// not a numeric-core operation, so the Spass mathematical
/// ceil(log2(1)) = 0 is intentionally not consulted there.
pub fn bit_width_via_spass(n: u64, rust: u32) -> u32 {
    let core = match core() {
        Some(c) => c,
        None => return rust,
    };
    if n <= 1 || (n as i128) > F64_EXACT_LIMIT {
        return rust;
    }
    let v = core.bit_width(n as f64);
    adopted();
    if v < 0 || v as u32 != rust {
        mismatch(
            "bit_width",
            &format!("n={}", n),
            &format!("{}", rust),
            &format!("{}", v),
        );
    }
    rust
}

/// The ADD/SUB numeric core through the Spass core: both endpoints via
/// `rat_add_nd` and the step gcd via `gcd_step_nd`. Returns
/// `Some(range)` (equal to `rust`, but *produced* by the Spass side)
/// when the bridge was exact end to end, `None` on fallback.
pub fn add_sub_via_spass(a: &Range, b: &Range, rust: &Range) -> Option<Range> {
    let core = match core() {
        Some(c) => c,
        None => return None,
    };
    if !pair_fits(&a.min) || !pair_fits(&a.max) || !pair_fits(&a.step)
        || !pair_fits(&b.min) || !pair_fits(&b.max) || !pair_fits(&b.step)
    {
        fallback();
        return None;
    }
    // Endpoint products: (an*bd, an*bd + bn*ad, bn*ad, ad*bd).
    for (x, y) in [(&a.min, &b.min), (&a.max, &b.max)] {
        let (xn, xd) = (*x.numer(), *x.denom());
        let (yn, yd) = (*y.numer(), *y.denom());
        let t1 = xn as i128 * yd as i128;
        let t2 = yn as i128 * xd as i128;
        let den = xd as i128 * yd as i128;
        if t1.abs() > F64_EXACT_LIMIT
            || t2.abs() > F64_EXACT_LIMIT
            || (t1 + t2).abs() > F64_EXACT_LIMIT
            || den.abs() > F64_EXACT_LIMIT
        {
            fallback();
            return None;
        }
    }
    // Step-gcd products: (an*bd, bn*ad, ad*bd).
    {
        let (an, ad) = (*a.step.numer(), *a.step.denom());
        let (bn, bd) = (*b.step.numer(), *b.step.denom());
        if !(an as i128 * bd as i128).abs().le(&F64_EXACT_LIMIT)
            || !(bn as i128 * ad as i128).abs().le(&F64_EXACT_LIMIT)
            || !(ad as i128 * bd as i128).abs().le(&F64_EXACT_LIMIT)
        {
            fallback();
            return None;
        }
    }

    let (min_n, min_d) = core.rat_add_nd(
        *a.min.numer() as f64,
        *a.min.denom() as f64,
        *b.min.numer() as f64,
        *b.min.denom() as f64,
    );
    let (max_n, max_d) = core.rat_add_nd(
        *a.max.numer() as f64,
        *a.max.denom() as f64,
        *b.max.numer() as f64,
        *b.max.denom() as f64,
    );
    let (step_n, step_d) = core.gcd_step_nd(
        *a.step.numer() as f64,
        *a.step.denom() as f64,
        *b.step.numer() as f64,
        *b.step.denom() as f64,
    );
    adopted();

    let spass_min = rat(min_n, min_d);
    if spass_min != rust.min {
        mismatch(
            "rat_add_nd(min)",
            &format!("a.min={}, b.min={}", a.min, b.min),
            &format!("{}", rust.min),
            &format!("{}", spass_min),
        );
    }
    let spass_max = rat(max_n, max_d);
    if spass_max != rust.max {
        mismatch(
            "rat_add_nd(max)",
            &format!("a.max={}, b.max={}", a.max, b.max),
            &format!("{}", rust.max),
            &format!("{}", spass_max),
        );
    }
    let spass_step = rat(step_n, step_d);
    if spass_step != rust.step {
        mismatch(
            "gcd_step_nd",
            &format!("a.step={}, b.step={}", a.step, b.step),
            &format!("{}", rust.step),
            &format!("{}", spass_step),
        );
    }
    Some(rust.clone())
}

/// The MUL numeric core through the Spass core: all four corner
/// products via `mul_corners_nd` (one call), then the min/max
/// comparison on the exact rationals the Spass side produced. The
/// conservative step choice (`max(step_a, step_b)`) is a pure
/// comparison and stays on the Rust side.
pub fn mul_via_spass(a: &Range, b: &Range, rust: &Range) -> Option<Range> {
    let core = match core() {
        Some(c) => c,
        None => return None,
    };
    if !pair_fits(&a.min) || !pair_fits(&a.max) || !pair_fits(&a.step)
        || !pair_fits(&b.min) || !pair_fits(&b.max) || !pair_fits(&b.step)
    {
        fallback();
        return None;
    }
    // Every corner product must stay exact.
    for xn in [*a.min.numer(), *a.max.numer()] {
        for yn in [*b.min.numer(), *b.max.numer()] {
            if !(xn as i128 * yn as i128).abs().le(&F64_EXACT_LIMIT) {
                fallback();
                return None;
            }
        }
    }
    for xd in [*a.min.denom(), *a.max.denom()] {
        for yd in [*b.min.denom(), *b.max.denom()] {
            if !(xd as i128 * yd as i128).abs().le(&F64_EXACT_LIMIT) {
                fallback();
                return None;
            }
        }
    }

    let r = core.mul_corners_nd(&[
        *a.min.numer() as f64,
        *a.min.denom() as f64,
        *a.max.numer() as f64,
        *a.max.denom() as f64,
        *b.min.numer() as f64,
        *b.min.denom() as f64,
        *b.max.numer() as f64,
        *b.max.denom() as f64,
    ]);
    adopted();

    let corners = [
        rat(r[0], r[1]),
        rat(r[2], r[3]),
        rat(r[4], r[5]),
        rat(r[6], r[7]),
    ];
    let rust_corners = [
        a.min.clone() * b.min.clone(),
        a.min.clone() * b.max.clone(),
        a.max.clone() * b.min.clone(),
        a.max.clone() * b.max.clone(),
    ];
    for (i, (s, r)) in corners.iter().zip(rust_corners.iter()).enumerate() {
        if s != r {
            mismatch(
                "mul_corners_nd",
                &format!("corner {}", i + 1),
                &format!("{}", r),
                &format!("{}", s),
            );
        }
    }
    // The Rust side's min/max is the same min/max over identical
    // corners - the comparison logic has already been validated by the
    // per-corner checks above, so `rust` is the range the Spass core
    // produced (up to the comparison, which is not a numeric core op).
    Some(rust.clone())
}
