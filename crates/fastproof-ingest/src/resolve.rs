//! Resolution of attachment references to files of the upload.

use std::collections::{HashMap, HashSet};

/// Index of the regular files of an upload.
#[derive(Default)]
pub(crate) struct FileIndex {
    paths: HashSet<String>,
    by_name: HashMap<String, Vec<String>>,
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

impl FileIndex {
    pub(crate) fn insert(&mut self, path: &str) {
        if self.paths.insert(path.to_string()) {
            let name = path.rsplit('/').next().unwrap_or(path).to_string();
            self.by_name.entry(name).or_default().push(path.to_string());
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
        let parts: Vec<&str> = r
            .split('/')
            .filter(|p| !p.is_empty() && *p != "." && *p != "..")
            .collect();
        let name = parts.last()?;
        let candidates = self.by_name.get(*name)?;
        let mut best: Option<(&String, usize)> = None;
        for c in candidates {
            let cparts: Vec<&str> = c.split('/').collect();
            let common = cparts
                .iter()
                .rev()
                .zip(parts.iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            match best {
                Some((_, n)) if n >= common => {}
                _ => best = Some((c, common)),
            }
        }
        // A relative reference must match completely; an absolute one by its tail.
        best.filter(|(_, n)| absolute || *n == parts.len())
            .map(|(c, _)| c.clone())
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
}
