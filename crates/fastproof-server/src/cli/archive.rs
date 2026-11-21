//! tar.gz of result files for uploads: streamed for a directory, in memory for watch batches.

use std::io::Write;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use flate2::Compression;
use flate2::write::GzEncoder;
use tokio::sync::mpsc;

/// Regular files under `dir` (symbolic links are skipped), as (absolute, relative) paths.
pub fn files_under(dir: &Path, max: usize) -> std::io::Result<Vec<(PathBuf, String)>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&d)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let ft = e.file_type()?;
            if ft.is_symlink() {
                continue;
            }
            let p = e.path();
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() {
                let rel = p
                    .strip_prefix(dir)
                    .map(|r| r.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                if !rel.is_empty() {
                    out.push((p, rel));
                }
                if out.len() > max {
                    return Err(std::io::Error::other(format!("more than {max} files")));
                }
            }
        }
    }
    Ok(out)
}

/// Writes a tar.gz of `files` into `w`.
pub fn write_tar_gz<W: Write>(files: &[(PathBuf, String)], w: W) -> std::io::Result<W> {
    let gz = GzEncoder::new(w, Compression::fast());
    let mut tar = tar::Builder::new(gz);
    tar.mode(tar::HeaderMode::Deterministic);
    tar.follow_symlinks(false);
    for (abs, rel) in files {
        let mut f = match std::fs::File::open(abs) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        tar.append_file(rel, &mut f)?;
    }
    tar.into_inner()?.finish()
}

/// In-memory tar.gz.
pub fn tar_gz_bytes(files: &[(PathBuf, String)]) -> std::io::Result<Vec<u8>> {
    write_tar_gz(files, Vec::new())
}

struct ChannelWriter {
    tx: mpsc::Sender<Result<Bytes, std::io::Error>>,
    buf: Vec<u8>,
}

impl Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= 256 * 1024 {
            self.flush()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.buf.is_empty() {
            let chunk = Bytes::from(std::mem::take(&mut self.buf));
            self.tx
                .blocking_send(Ok(chunk))
                .map_err(|_| std::io::Error::other("the upload was aborted"))?;
        }
        Ok(())
    }
}

/// A tar.gz of a directory produced while it is sent.
pub fn stream_dir(dir: PathBuf) -> reqwest::Body {
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    tokio::task::spawn_blocking(move || {
        let r = files_under(&dir, 1_000_000).and_then(|files| {
            let mut w = write_tar_gz(
                &files,
                ChannelWriter {
                    tx: tx.clone(),
                    buf: Vec::new(),
                },
            )?;
            w.flush()
        });
        if let Err(e) = r {
            let _ = tx.blocking_send(Err(e));
        }
    });
    reqwest::Body::wrap_stream(tokio_stream::wrappers::ReceiverStream::new(rx))
}
