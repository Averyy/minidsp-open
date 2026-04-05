//! Biquad filter coefficient validation and utilities.
//!
//! MiniDSP uses standard IIR biquad form: `y[n] = b0*x[n] + b1*x[n-1] + b2*x[n-2] - a1*y[n-1] - a2*y[n-2]`
//! Coefficients: [b0, b1, b2, a1, a2]
//!
//! Stability: a filter is stable when all poles are inside the unit circle.
//! We check that the transfer function denominator `1 + a1*z^-1 + a2*z^-2` has
//! roots with magnitude < 1.

/// Standard bypass (passthrough) biquad: b0=1, all others 0.
pub const BYPASS: [f32; 5] = [1.0, 0.0, 0.0, 0.0, 0.0];

/// Check if biquad coefficients represent a stable filter.
///
/// Returns `true` if the filter is stable (all poles inside unit circle).
/// A filter with coefficients [b0, b1, b2, a1, a2] has denominator
/// `1 + a1*z^-1 + a2*z^-2`. Stability requires:
/// - |a2| < 1
/// - |a1| < 1 + a2
pub fn is_stable(coeffs: &[f32; 5]) -> bool {
    let a1 = coeffs[3] as f64;
    let a2 = coeffs[4] as f64;
    a2.abs() < 1.0 && a1.abs() < 1.0 + a2
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
        assert!(!is_bypass(&[1.0003468, -1.9990215, 0.9986773, -1.9990215, 0.9990241]));
    }

    #[test]
    fn typical_peq_is_stable() {
        // Typical PEQ from ezBEQ
        let coeffs = [1.0003468, -1.9990215, 0.9986773, -1.9990215, 0.9990241];
        assert!(is_stable(&coeffs));
    }

    #[test]
    fn unstable_filter_detected() {
        // a2 > 1 → unstable
        let coeffs = [1.0, 0.0, 0.0, 0.0, 1.5];
        assert!(!is_stable(&coeffs));
    }
}
