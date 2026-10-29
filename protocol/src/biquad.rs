//! Biquad filter coefficient validation and utilities.
//!
//! MiniDSP uses standard IIR biquad form: `y[n] = b0*x[n] + b1*x[n-1] + b2*x[n-2] - a1*y[n-1] - a2*y[n-2]`
//! Coefficients: [b0, b1, b2, a1, a2]
//!
//! Stability: a filter is stable when all poles are inside the unit circle.
//! We support two feedback-sign conventions in the ecosystem:
//! - direct-form coefficients where the denominator is `1 + a1*z^-1 + a2*z^-2`
//! - stored/round-tripped DSP coefficients where the feedback terms are already
//!   negated and the denominator is `1 - a1*z^-1 - a2*z^-2`

/// Standard bypass (passthrough) biquad: b0=1, all others 0.
pub const BYPASS: [f32; 5] = [1.0, 0.0, 0.0, 0.0, 0.0];

fn jury_stable(a1: f64, a2: f64) -> bool {
    a2.abs() < 1.0 && (1.0 + a1 + a2) > 0.0 && (1.0 - a1 + a2) > 0.0
}

/// Check if biquad coefficients are stable using the standard coefficient convention.
///
/// A filter with coefficients `[b0, b1, b2, a1, a2]` has denominator
/// `1 + a1*z^-1 + a2*z^-2`.
pub fn is_stable(coeffs: &[f32; 5]) -> bool {
    let a1 = coeffs[3] as f64;
    let a2 = coeffs[4] as f64;
    jury_stable(a1, a2)
}

/// Check if the coefficients are stable under either common feedback-sign convention.
///
/// This is useful for non-fatal warnings when we may be round-tripping raw
/// device coefficients back into the CLI/API.
pub fn is_stable_any_convention(coeffs: &[f32; 5]) -> bool {
    let a1 = coeffs[3] as f64;
    let a2 = coeffs[4] as f64;
    jury_stable(a1, a2) || jury_stable(-a1, -a2)
}

/// Check if coefficients represent a bypass (passthrough) filter.
pub fn is_bypass(coeffs: &[f32; 5]) -> bool {
    (coeffs[0] - 1.0).abs() < f32::EPSILON
        && coeffs[1].abs() < f32::EPSILON
        && coeffs[2].abs() < f32::EPSILON
        && coeffs[3].abs() < f32::EPSILON
        && coeffs[4].abs() < f32::EPSILON
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bypass_is_stable() {
        assert!(is_stable(&BYPASS));
    }

    #[test]
    fn bypass_detected() {
        assert!(is_bypass(&BYPASS));
        assert!(!is_bypass(&[
            1.0003468, -1.9990215, 0.9986773, -1.9990215, 0.9990241
        ]));
    }

    #[test]
    fn typical_peq_is_stable() {
        // Typical PEQ from ezBEQ
        let coeffs = [1.0003468, -1.9990215, 0.9986773, -1.9990215, 0.9990241];
        assert!(is_stable(&coeffs));
        assert!(is_stable_any_convention(&coeffs));
    }

    #[test]
    fn round_tripped_device_biquad_is_stable_in_alternate_convention() {
        let coeffs = [
            0.000_010_644_801_5,
            0.000_021_289_603,
            0.000_010_644_801_5,
            1.987_936_6,
            -0.987_979_23,
        ];
        assert!(!is_stable(&coeffs));
        assert!(is_stable_any_convention(&coeffs));
    }

    #[test]
    fn unstable_filter_detected() {
        // a2 > 1 → unstable
        let coeffs = [1.0, 0.0, 0.0, 0.0, 1.5];
        assert!(!is_stable(&coeffs));
    }
}
