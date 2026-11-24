//! The state file of `rp import`: which launches are replayed completely, so that an interrupted
//! import continues where it stopped and a repeated one skips what is done.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

const VERSION: u32 = 1;

/// What a replayed launch brought.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Done {
    pub name: String,
    pub items: u64,
    pub logs: u64,
    pub files: u64,
    pub at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateFile {
    pub version: u32,
    pub rp_url: String,
    pub rp_project: String,
    pub project: String,
    pub launches: BTreeMap<String, Done>,
}

/// The state file with its path.
#[derive(Debug)]
pub struct State {
    path: PathBuf,
    pub file: StateFile,
}

impl State {
    /// Opens the file (a missing one is empty). It must belong to the same source and target.
    pub fn open(
        path: &Path,
        rp_url: &str,
        rp_project: &str,
        project: &str,
    ) -> anyhow::Result<Self> {
        let mut file = match std::fs::read(path) {
            Ok(b) => serde_json::from_slice::<StateFile>(&b).with_context(|| {
                format!("{} is not a state file of `rp import`", path.display())
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => StateFile::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        if file.version == 0 {
            file.version = VERSION;
            file.rp_url = rp_url.to_string();
            file.rp_project = rp_project.to_string();
            file.project = project.to_string();
        } else if file.version != VERSION {
            bail!(
                "{}: unknown state file version {}",
                path.display(),
                file.version
            );
        } else if file.rp_url != rp_url || file.rp_project != rp_project || file.project != project
        {
            bail!(
                "{} belongs to another import ({} / {} into {}); name another file with --state",
                path.display(),
                file.rp_url,
                file.rp_project,
                file.project
            );
        }
        Ok(Self {
            path: path.to_path_buf(),
            file,
        })
    }

    pub fn is_done(&self, uuid: &str) -> bool {
        self.file.launches.contains_key(uuid)
    }

    /// Records a replayed launch and writes the file (to a temporary file, then renamed).
    pub fn mark_done(&mut self, uuid: &str, done: Done) -> anyhow::Result<()> {
        self.file.launches.insert(uuid.to_string(), done);
        self.save()
    }

    fn save(&self) -> anyhow::Result<()> {
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.file)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("writing {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_resumes_and_refuses_a_foreign_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/state.json");
        let mut s = State::open(&p, "https://rp.example.com/", "team", "shop").unwrap();
        assert!(!s.is_done("a"));
        s.mark_done(
            "a",
            Done {
                name: "n".into(),
                items: 3,
                ..Default::default()
            },
        )
        .unwrap();
        let s = State::open(&p, "https://rp.example.com/", "team", "shop").unwrap();
        assert!(s.is_done("a"));
        assert_eq!(s.file.launches["a"].items, 3);
        let e = State::open(&p, "https://rp.example.com/", "other", "shop").unwrap_err();
        assert!(e.to_string().contains("belongs to another import"), "{e}");
        std::fs::write(&p, "not json").unwrap();
        assert!(State::open(&p, "u", "t", "s").is_err());
    }
}
