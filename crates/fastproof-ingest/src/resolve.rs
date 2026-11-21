//! Resolution of attachment references to files of the upload.

use std::cell::OnceCell;
use std::cmp::Ordering;
use std::collections::HashSet;

/// Index of the regular files of an upload.
#[derive(Default)]
pub(crate) struct FileIndex {
    paths: HashSet<String>,
    /// Paths in insertion order.
    order: Vec<String>,
    /// Built on the first suffix lookup, after all files were inserted.
    suffixes: OnceCell<SuffixIndex>,
}

fn lexical(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

/// Files sorted by their reversed path components, so all files sharing a path suffix form
/// one contiguous range, plus a sparse table answering "first inserted file in a range".
/// A lookup costs O(log n) comparisons instead of a scan of every file with the same name.
struct SuffixIndex {
    /// Insertion indexes, sorted by reversed components.
    sorted: Vec<u32>,
    /// `min[j][i]`: smallest insertion index in `sorted[i..i + 2^j]`.
    min: Vec<Vec<u32>>,
}

/// Compares the reversed components of `path` with `query` (already reversed), looking at
/// no more than `limit` components of `path`.
fn cmp_reversed(path: &str, query: &[&str], limit: usize) -> Ordering {
    path.rsplit('/')
        .take(limit)
        .cmp(query.iter().copied().take(limit))
}

fn common_suffix(path: &str, query: &[&str]) -> usize {
    path.rsplit('/')
        .zip(query.iter())
        .take_while(|(a, b)| a == *b)
        .count()
}

impl SuffixIndex {
    fn build(order: &[String]) -> SuffixIndex {
        let mut sorted: Vec<u32> = (0..order.len() as u32).collect();
        sorted.sort_by(|&a, &b| {
            order[a as usize]
                .rsplit('/')
                .cmp(order[b as usize].rsplit('/'))
                .then(a.cmp(&b))
        });
        let mut min = vec![sorted.clone()];
        let mut width = 1usize;
        while width * 2 <= sorted.len() {
            let prev = &min[min.len() - 1];
            let next: Vec<u32> = (0..=sorted.len() - width * 2)
                .map(|i| prev[i].min(prev[i + width]))
                .collect();
            min.push(next);
            width *= 2;
        }
        SuffixIndex { sorted, min }
    }

    /// Smallest insertion index in `sorted[lo..hi]` (non-empty).
    fn first_in(&self, lo: usize, hi: usize) -> u32 {
        let level = (usize::BITS - 1 - (hi - lo).leading_zeros()) as usize;
        let row = &self.min[level];
        row[lo].min(row[hi - (1 << level)])
    }

    /// File with the longest common path suffix with `query` (reversed components); among
    /// equally long matches the first inserted one. Returns the file and the suffix length.
    fn best(&self, order: &[String], query: &[&str]) -> Option<(u32, usize)> {
        let path = |i: usize| order[self.sorted[i] as usize].as_str();
        let pos = self
            .sorted
            .partition_point(|&i| cmp_reversed(&order[i as usize], query, usize::MAX).is_lt());
        let mut common = 0;
        if pos > 0 {
            common = common_suffix(path(pos - 1), query);
        }
        if pos < self.sorted.len() {
            common = common.max(common_suffix(path(pos), query));
        }
        if common == 0 {
            return None;
        }
        let lo = self
            .sorted
            .partition_point(|&i| cmp_reversed(&order[i as usize], query, common).is_lt());
        let hi = self
            .sorted
            .partition_point(|&i| cmp_reversed(&order[i as usize], query, common).is_le());
        Some((self.first_in(lo, hi), common))
    }
}

impl FileIndex {
    pub(crate) fn insert(&mut self, path: &str) {
        if self.paths.insert(path.to_string()) {
            self.order.push(path.to_string());
            self.suffixes = OnceCell::new();
        }
    }

    /// Resolves `reference` (as written by the writer) given the directory of the report
    /// that references it and an optional pre-computed candidate.
    pub(crate) fn resolve(
        &self,
        reference: &str,
        candidate: Option<&str>,
        report_dir: &str,
    ) -> Option<String> {
        if let Some(c) = candidate.and_then(lexical)
            && self.paths.contains(&c)
        {
            return Some(c);
        }
        let r = reference.trim().replace('\\', "/");
        let r = r.strip_prefix("file://").unwrap_or(&r).to_string();
        let absolute = r.starts_with('/') || (r.len() > 2 && r.as_bytes()[1] == b':');
        if !absolute {
            let joined = if report_dir.is_empty() {
                r.clone()
            } else {
                format!("{report_dir}/{r}")
            };
            if let Some(j) = lexical(&joined)
                && self.paths.contains(&j)
            {
                return Some(j);
            }
            if let Some(j) = lexical(&r)
                && self.paths.contains(&j)
            {
                return Some(j);
            }
        }
        // Absolute paths from the machine that ran the tests (Playwright JSON, CTRF): match
        // the longest path suffix among files with the same name.
        let reversed: Vec<&str> = r
            .rsplit('/')
            .filter(|p| !p.is_empty() && *p != "." && *p != "..")
            .collect();
        if reversed.is_empty() {
            return None;
        }
        let index = self
            .suffixes
            .get_or_init(|| SuffixIndex::build(&self.order));
        let (file, common) = index.best(&self.order, &reversed)?;
        // A relative reference must match completely; an absolute one by its tail.
        (absolute || common == reversed.len()).then(|| self.order[file as usize].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves() {
        let mut idx = FileIndex::default();
        for p in [
            "allure-results/a-attachment.png",
            "out/test-results/t1/shot.png",
            "out/test-results/t2/shot.png",
            "out/report.json",
        ] {
            idx.insert(p);
        }
        assert_eq!(
            idx.resolve(
                "a-attachment.png",
                Some("allure-results/a-attachment.png"),
                "allure-results"
            )
            .as_deref(),
            Some("allure-results/a-attachment.png")
        );
        assert_eq!(
            idx.resolve("/builds/x/y/test-results/t2/shot.png", None, "out")
                .as_deref(),
            Some("out/test-results/t2/shot.png")
        );
        assert_eq!(
            idx.resolve("test-results/t1/shot.png", None, "out")
                .as_deref(),
            Some("out/test-results/t1/shot.png")
        );
        assert_eq!(
            idx.resolve("../test-results/t1/shot.png", None, "out/sub")
                .as_deref(),
            Some("out/test-results/t1/shot.png")
        );
        assert_eq!(idx.resolve("missing.png", None, "out"), None);
    }

    #[test]
    fn suffix_ties_pick_the_first_inserted_file() {
        let mut idx = FileIndex::default();
        for p in ["z/b/shot.png", "a/b/shot.png", "c/shot.png", "b/c/d/e.txt"] {
            idx.insert(p);
        }
        assert_eq!(
            idx.resolve("/ci/x/shot.png", None, "").as_deref(),
            Some("z/b/shot.png")
        );
        assert_eq!(
            idx.resolve("/ci/b/shot.png", None, "").as_deref(),
            Some("z/b/shot.png")
        );
        assert_eq!(
            idx.resolve("/ci/c/shot.png", None, "").as_deref(),
            Some("c/shot.png")
        );
        assert_eq!(
            idx.resolve("/ci/c/d/e.txt", None, "").as_deref(),
            Some("b/c/d/e.txt")
        );
        assert_eq!(idx.resolve("q/shot.png", None, "out"), None);
        assert_eq!(idx.resolve("/ci/other.png", None, ""), None);
    }

    /// Many files with the same name and as many absolute references: the old scan
    /// compared every reference with every file (n^2 path splits).
    #[test]
    fn suffix_matching_is_not_quadratic() {
        let n = 20_000;
        let mut idx = FileIndex::default();
        for i in 0..n {
            idx.insert(&format!("out/test-results/t{i}/shot.png"));
        }
        let t = std::time::Instant::now();
        for i in 0..n {
            let r = format!("/builds/ci/test-results/t{i}/shot.png");
            assert_eq!(
                idx.resolve(&r, None, "out").as_deref(),
                Some(format!("out/test-results/t{i}/shot.png").as_str())
            );
        }
        for i in 0..n {
            assert_eq!(
                idx.resolve(&format!("/elsewhere/x{i}/shot.png"), None, "out")
                    .as_deref(),
                Some("out/test-results/t0/shot.png")
            );
        }
        let took = t.elapsed();
        assert!(took.as_secs() < 10, "{n} lookups took {took:?}");
    }
}
