//! The same fixture read as a directory, zip, tar and tar.gz gives the same batch.

use std::io::Write;
use std::path::{Path, PathBuf};

use fastproof_ingest::{IngestOptions, MemorySink, UploadKind, ingest_path};

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(rel)
}

fn files(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            files(&p, base, out);
        } else {
            let rel = p
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, std::fs::read(&p).unwrap()));
        }
    }
}

fn summary(path: &Path) -> (UploadKind, String) {
    let mut sink = MemorySink::default();
    let b = ingest_path(path, &IngestOptions::default(), &mut sink).unwrap();
    let mut results = b.results.clone();
    for r in &mut results {
        r.visit_attachments_mut(&mut |a| {
            let body = &sink.get(a.id.as_ref().unwrap()).unwrap().bytes;
            a.id = Some(fastproof_model::AttachmentId(format!("len:{}", body.len())));
        });
    }
    results.sort_by(|a, b| a.source_file.cmp(&b.source_file));
    (
        b.upload_kind,
        serde_json::to_string(&(results, &b.containers.len(), &b.launch_meta, &b.categories))
            .unwrap(),
    )
}

#[test]
fn directory_zip_tar_and_tgz_agree() {
    let dir = fixture("pytest/allure-results");
    let mut entries = Vec::new();
    files(&dir, &dir, &mut entries);
    let t = tempfile::TempDir::new().unwrap();

    let zip_path = t.path().join("u.zip");
    {
        let mut z = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        z.add_directory("sub/", o).unwrap();
        for (name, data) in &entries {
            z.start_file(name.as_str(), o).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
    }
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in &entries {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        builder
            .append_data(&mut h, format!("./{name}"), data.as_slice())
            .unwrap();
    }
    let tar_bytes = builder.into_inner().unwrap();
    let tar_path = t.path().join("u.tar");
    std::fs::write(&tar_path, &tar_bytes).unwrap();
    let tgz_path = t.path().join("u.tar.gz");
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(&tar_bytes).unwrap();
    std::fs::write(&tgz_path, e.finish().unwrap()).unwrap();

    let (k, base) = summary(&dir);
    assert_eq!(k, UploadKind::Directory);
    for (path, kind) in [
        (zip_path, UploadKind::Zip),
        (tar_path, UploadKind::Tar),
        (tgz_path, UploadKind::TarGz),
    ] {
        let (k, s) = summary(&path);
        assert_eq!(k, kind);
        assert_eq!(s, base, "{kind:?}");
    }
}

#[test]
fn single_report_files() {
    for (rel, n) in [
        ("pytest/junit-xunit2.xml", 16),
        ("playwright/results.json", 16),
        ("playwright/ctrf/ctrf-report.json", 16),
        ("pytest/allure-results/categories.json", 0),
    ] {
        let b = ingest_path(
            &fixture(rel),
            &IngestOptions::default(),
            &mut MemorySink::default(),
        )
        .unwrap();
        assert_eq!(b.upload_kind, UploadKind::File);
        assert_eq!(b.results.len(), n, "{rel}");
        if rel.ends_with("results.json") || rel.ends_with("ctrf-report.json") {
            assert!(
                b.counts.attachments_missing > 0,
                "attachments of a single report arrive separately"
            );
        }
    }
}
