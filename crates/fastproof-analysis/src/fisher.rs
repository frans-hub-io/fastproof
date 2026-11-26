//! Fisher's exact test on a 2×2 table, two-sided, for the flaky-cause classifiers (contract
//! §7.8): "does this test fail more often on host X than elsewhere?" with the small counts a
//! series window has (5 to 50 runs), where a chi-squared approximation is wrong.
//!
//! The table is
//!
//! ```text
//!              failed   passed
//! this value     a        b
//! the others     c        d
//! ```
//!
//! and the p-value is the sum of the hypergeometric probabilities of every table with the same
//! margins that is at most as likely as the observed one (the definition R's `fisher.test`
//! uses). Probabilities are computed in log space, so large counts neither overflow nor lose
//! the small terms.

/// Relative tolerance when comparing a table's probability with the observed one (as R).
const TOLERANCE: f64 = 1e-7;

/// Natural logarithm of the gamma function (Lanczos, g = 7, n = 9; relative error < 1e-13 for
/// the positive arguments used here).
fn ln_gamma(x: f64) -> f64 {
    const G: f64 = 7.0;
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        let pi = std::f64::consts::PI;
        return (pi / (pi * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = C[0];
    let t = x + G + 0.5;
    for (i, c) in C.iter().enumerate().skip(1) {
        a += c / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

/// `ln(n!)`, exact summation for small `n`.
fn ln_factorial(n: u64) -> f64 {
    if n < 2 {
        return 0.0;
    }
    if n <= 64 {
        return (2..=n).map(|k| (k as f64).ln()).sum();
    }
    ln_gamma(n as f64 + 1.0)
}

fn ln_choose(n: u64, k: u64) -> f64 {
    ln_factorial(n) - ln_factorial(k) - ln_factorial(n - k)
}

/// Two-sided p-value of Fisher's exact test for the table `[[a, b], [c, d]]`. A table with an
/// empty row or column carries no evidence and answers 1.
pub fn two_sided(a: u64, b: u64, c: u64, d: u64) -> f64 {
    let r1 = a + b;
    let r2 = c + d;
    let c1 = a + c;
    let n = r1 + r2;
    if r1 == 0 || r2 == 0 || c1 == 0 || c1 == n {
        return 1.0;
    }
    let ln_total = ln_choose(n, c1);
    let ln_p = |x: u64| ln_choose(r1, x) + ln_choose(r2, c1 - x) - ln_total;
    let observed = ln_p(a);
    let lo = c1.saturating_sub(r2);
    let hi = r1.min(c1);
    let limit = observed + TOLERANCE.ln_1p();
    let mut p = 0.0;
    for x in lo..=hi {
        let l = ln_p(x);
        if l <= limit {
            p += l.exp();
        }
    }
    p.clamp(0.0, 1.0)
}

/// One-sided p-value that the first row fails more often than the second (`P(X ≥ a)`), for
/// the evidence shown next to a hint.
pub fn greater(a: u64, b: u64, c: u64, d: u64) -> f64 {
    let r1 = a + b;
    let r2 = c + d;
    let c1 = a + c;
    let n = r1 + r2;
    if r1 == 0 || r2 == 0 || c1 == 0 || c1 == n {
        return 1.0;
    }
    let ln_total = ln_choose(n, c1);
    let hi = r1.min(c1);
    (a..=hi)
        .map(|x| (ln_choose(r1, x) + ln_choose(r2, c1 - x) - ln_total).exp())
        .sum::<f64>()
        .clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, rel: f64) -> bool {
        (a - b).abs() <= rel * b.abs().max(1e-300)
    }

    #[test]
    fn known_values_of_r() {
        // Exact values (rational arithmetic), as fisher.test(matrix(c(a, c, b, d), 2)) in R.
        let cases: &[(u64, u64, u64, u64, f64)] = &[
            (3, 1, 1, 3, 0.485_714_285_714_285_7),
            (1, 9, 11, 3, 0.002_759_456_185_220_084),
            (0, 5, 5, 0, 0.007_936_507_936_507_936),
            (5, 0, 0, 5, 0.007_936_507_936_507_936),
            (10, 10, 10, 10, 1.0),
            (7, 3, 2, 18, 0.001_657_213_351_366_275),
            (2, 3, 1, 4, 1.0),
        ];
        for &(a, b, c, d, want) in cases {
            let got = two_sided(a, b, c, d);
            assert!(close(got, want, 1e-9), "({a},{b},{c},{d}): {got} != {want}");
        }
    }

    #[test]
    fn boundaries() {
        assert_eq!(two_sided(0, 0, 0, 0), 1.0);
        assert_eq!(two_sided(5, 0, 0, 0), 1.0);
        assert_eq!(two_sided(0, 0, 3, 4), 1.0);
        assert_eq!(two_sided(4, 0, 6, 0), 1.0);
        assert_eq!(two_sided(0, 4, 0, 6), 1.0);
        let one = two_sided(1, 0, 0, 1);
        assert!(close(one, 1.0, 1e-12), "{one}");
        // Symmetric: swapping rows or columns does not change a two-sided p.
        assert!(close(two_sided(7, 3, 2, 18), two_sided(2, 18, 7, 3), 1e-12));
        assert!(close(two_sided(7, 3, 2, 18), two_sided(3, 7, 18, 2), 1e-12));
        // The smallest table that can reach p < 0.05: 4 of 4 failing against 0 of 4.
        assert!(two_sided(4, 0, 0, 4) < 0.05);
        assert!(two_sided(3, 0, 0, 3) > 0.05);
    }

    #[test]
    fn stable_for_large_counts() {
        let p = two_sided(900, 100, 100, 900);
        assert!((0.0..1e-200).contains(&p), "{p}");
        let q = two_sided(5000, 5000, 5000, 5000);
        assert!(close(q, 1.0, 1e-9), "{q}");
        let r = two_sided(60, 940, 20, 980);
        assert!(r > 0.0 && r < 1e-4, "{r}");
    }

    #[test]
    fn one_sided_greater() {
        // Here the other tail adds nothing: every table of the lower tail is more likely.
        let p = greater(7, 3, 2, 18);
        assert!(close(p, 0.001_657_213_351_366_275, 1e-9), "{p}");
        assert!(greater(2, 18, 7, 3) > 0.99);
        assert_eq!(greater(0, 0, 1, 1), 1.0);
    }

    #[test]
    fn ln_gamma_matches_factorials() {
        for n in 1u64..=170 {
            let exact: f64 = (2..=n).map(|k| (k as f64).ln()).sum();
            assert!(
                close(ln_gamma(n as f64 + 1.0), exact, 1e-11) || exact == 0.0,
                "{n}"
            );
        }
    }
}
