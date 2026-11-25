//! Stack frames kept in the triage memory and their overlap.

use std::collections::HashSet;

/// Frames kept per memory row and compared, at most.
pub const MEMORY_FRAMES: usize = 10;

/// The first [`MEMORY_FRAMES`] non-framework frames of a trace
/// (`fastproof_model::fingerprint::stack_frames`).
pub fn frames(trace: &str) -> Vec<String> {
    let mut f = fastproof_model::fingerprint::stack_frames(trace);
    f.truncate(MEMORY_FRAMES);
    f
}

/// Jaccard index of two frame lists (0 when both are empty).
pub fn jaccard(a: &[String], b: &[String]) -> f64 {
    let x: HashSet<&str> = a.iter().map(String::as_str).collect();
    let y: HashSet<&str> = b.iter().map(String::as_str).collect();
    let union = x.union(&y).count();
    if union == 0 {
        return 0.0;
    }
    x.intersection(&y).count() as f64 / union as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_cut_and_compared() {
        let trace: String = (0..30)
            .map(|i| format!("\tat com.example.shop.Service{i}.call(Service{i}.java:{i})\n"))
            .collect();
        let f = frames(&format!("java.lang.IllegalStateException: boom\n{trace}"));
        assert_eq!(f.len(), MEMORY_FRAMES);
        assert_eq!(jaccard(&f, &f), 1.0);
        assert_eq!(jaccard(&[], &[]), 0.0);
        let half: Vec<String> = f[..5].to_vec();
        assert!((jaccard(&f, &half) - 0.5).abs() < 1e-9);
    }
}
