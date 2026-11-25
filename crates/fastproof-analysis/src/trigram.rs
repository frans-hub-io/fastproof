//! Trigram similarity of normalized messages, the measure of `pg_trgm` `similarity()`. The
//! live path and the backtest both score with it, so their answers are the same.

use std::collections::BTreeSet;

fn trigrams(s: &str) -> BTreeSet<[char; 3]> {
    let mut out = BTreeSet::new();
    for word in s
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let chars: Vec<char> = "  "
            .chars()
            .chain(word.chars())
            .chain(std::iter::once(' '))
            .collect();
        for w in chars.windows(3) {
            out.insert([w[0], w[1], w[2]]);
        }
    }
    out
}

/// `pg_trgm` `similarity()`: shared trigrams over all trigrams of both strings.
pub fn similarity(a: &str, b: &str) -> f64 {
    let (x, y) = (trigrams(a), trigrams(b));
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    let shared = x.intersection(&y).count();
    shared as f64 / (x.len() + y.len() - shared) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_pg_trgm() {
        assert_eq!(similarity("word", "word"), 1.0);
        assert_eq!(similarity("", "word"), 0.0);
        // pg_trgm: similarity('word', 'two words') = 0.36363637
        assert!((similarity("word", "two words") - 0.363_636_37).abs() < 1e-6);
        let a = "Connection refused: connect to <tmp> port <n>";
        let b = "Connection refused: connect to db port <n>";
        assert!(similarity(a, b) > 0.5);
        assert!(similarity(a, "expected: <<n>> but was: <<n>>") < 0.3);
    }
}
