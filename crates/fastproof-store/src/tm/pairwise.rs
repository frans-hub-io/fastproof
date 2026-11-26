//! Pairwise generation of parameter rows (deterministic greedy AETG, §7.5, night 2).
//!
//! Every pair of values of two different parameters appears in at least one row. Each new row
//! is the best of [`CANDIDATES`] greedy candidates: a candidate starts from the parameter value
//! that takes part in the most uncovered pairs, then visits the other parameters in an order
//! drawn from a fixed-seed generator and gives each the value that covers the most uncovered
//! pairs with the values chosen so far (ties: the first value). The seed is fixed, so the same
//! input always gives the same rows.

/// Greedy candidates tried for every row.
pub const CANDIDATES: usize = 8;

/// A small fixed-seed generator (xorshift64*), only for the parameter order of candidates.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Uncovered pairs: for parameters `i < j`, one flag per value pair `(a, b)` at `a * n_j + b`.
struct Pairs {
    sizes: Vec<usize>,
    /// Index of the table of `(i, j)`, `i < j`, at `i * k + j`.
    table: Vec<usize>,
    flags: Vec<Vec<bool>>,
    left: usize,
}

impl Pairs {
    fn new(sizes: &[usize]) -> Self {
        let k = sizes.len();
        let mut table = vec![usize::MAX; k * k];
        let mut flags = Vec::new();
        let mut left = 0;
        for i in 0..k {
            for j in i + 1..k {
                table[i * k + j] = flags.len();
                flags.push(vec![true; sizes[i] * sizes[j]]);
                left += sizes[i] * sizes[j];
            }
        }
        Self {
            sizes: sizes.to_vec(),
            table,
            flags,
            left,
        }
    }

    fn uncovered(&self, p: usize, a: usize, q: usize, b: usize) -> bool {
        let k = self.sizes.len();
        let (i, x, j, y) = if p < q { (p, a, q, b) } else { (q, b, p, a) };
        self.flags[self.table[i * k + j]][x * self.sizes[j] + y]
    }

    fn cover(&mut self, row: &[usize]) -> usize {
        let k = self.sizes.len();
        let mut n = 0;
        for i in 0..k {
            for j in i + 1..k {
                let f = &mut self.flags[self.table[i * k + j]][row[i] * self.sizes[j] + row[j]];
                if *f {
                    *f = false;
                    n += 1;
                }
            }
        }
        self.left -= n;
        n
    }

    /// Uncovered pairs that value `a` of parameter `p` takes part in.
    fn degree(&self, p: usize, a: usize) -> usize {
        let mut n = 0;
        for q in 0..self.sizes.len() {
            if q != p {
                n += (0..self.sizes[q])
                    .filter(|&b| self.uncovered(p, a, q, b))
                    .count();
            }
        }
        n
    }
}

/// The smallest number of rows any pairwise set needs: the product of the two largest sizes
/// (one parameter: its size).
pub fn lower_bound(sizes: &[usize]) -> usize {
    let mut s: Vec<usize> = sizes.to_vec();
    s.sort_unstable_by(|a, b| b.cmp(a));
    match s.as_slice() {
        [] => 0,
        [a] => *a,
        [a, b, ..] => a.saturating_mul(*b),
    }
}

/// Rows of value indexes (one per parameter, in the order of `sizes`) that cover every pair of
/// values of two parameters; `None` once more than `max_rows` rows would be needed. Every
/// size is at least 1.
pub fn rows(sizes: &[usize], max_rows: usize) -> Option<Vec<Vec<usize>>> {
    let k = sizes.len();
    if k == 0 {
        return Some(Vec::new());
    }
    if sizes.contains(&0) {
        return Some(Vec::new());
    }
    if lower_bound(sizes) > max_rows {
        return None;
    }
    if k == 1 {
        return Some((0..sizes[0]).map(|a| vec![a]).collect());
    }
    let mut pairs = Pairs::new(sizes);
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut out: Vec<Vec<usize>> = Vec::new();
    while pairs.left > 0 {
        if out.len() >= max_rows {
            return None;
        }
        let mut first = (0, 0, 0);
        for (p, &n) in sizes.iter().enumerate() {
            for a in 0..n {
                let d = pairs.degree(p, a);
                if d > first.2 {
                    first = (p, a, d);
                }
            }
        }
        let mut best: Option<(usize, Vec<usize>)> = None;
        for _ in 0..CANDIDATES {
            let mut order: Vec<usize> = (0..k).filter(|&p| p != first.0).collect();
            for i in (1..order.len()).rev() {
                order.swap(i, rng.below(i + 1));
            }
            let mut row = vec![usize::MAX; k];
            row[first.0] = first.1;
            let mut set = vec![first.0];
            let mut gain = 0;
            for &p in &order {
                let mut pick = (0, 0);
                for a in 0..sizes[p] {
                    let g = set
                        .iter()
                        .filter(|&&q| pairs.uncovered(p, a, q, row[q]))
                        .count();
                    if g > pick.1 {
                        pick = (a, g);
                    }
                }
                row[p] = pick.0;
                gain += pick.1;
                set.push(p);
            }
            if best.as_ref().is_none_or(|b| gain > b.0) {
                best = Some((gain, row));
            }
        }
        let (_, row) = best?;
        if pairs.cover(&row) == 0 {
            return None;
        }
        out.push(row);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn covers_all(sizes: &[usize], rows: &[Vec<usize>]) -> bool {
        for i in 0..sizes.len() {
            for j in i + 1..sizes.len() {
                for a in 0..sizes[i] {
                    for b in 0..sizes[j] {
                        if !rows.iter().any(|r| r[i] == a && r[j] == b) {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    #[test]
    fn covers_every_pair_and_is_deterministic() {
        for sizes in [
            vec![2, 2],
            vec![3, 3, 3],
            vec![2, 3, 4],
            vec![3, 3, 3, 3],
            vec![5, 4, 3, 2, 2, 2],
            vec![2; 10],
            vec![10, 10, 2],
            vec![1, 4, 1],
        ] {
            let r = rows(&sizes, 1000).unwrap();
            assert!(covers_all(&sizes, &r), "{sizes:?}");
            assert_eq!(rows(&sizes, 1000).unwrap(), r, "{sizes:?}");
            assert!(r.len() >= lower_bound(&sizes));
            assert!(
                r.len() <= sizes.iter().product::<usize>(),
                "{sizes:?}: {} rows",
                r.len()
            );
        }
    }

    #[test]
    fn small_sets_stay_small() {
        assert!(rows(&[3, 3, 3, 3], 1000).unwrap().len() <= 11);
        assert!(rows(&[2; 10], 1000).unwrap().len() <= 10);
        assert_eq!(rows(&[2, 2], 1000).unwrap().len(), 4);
        assert_eq!(rows(&[4], 1000).unwrap().len(), 4);
        assert_eq!(rows(&[], 1000).unwrap().len(), 0);
    }

    #[test]
    fn refuses_beyond_the_row_limit() {
        assert!(
            rows(&[40, 30, 2], 1000).is_none(),
            "the lower bound is 1200"
        );
        assert!(rows(&[3, 3, 3, 3], 5).is_none());
        assert!(rows(&[3, 3, 3, 3], 20).is_some());
        assert_eq!(lower_bound(&[5, 7, 3]), 35);
    }
}
