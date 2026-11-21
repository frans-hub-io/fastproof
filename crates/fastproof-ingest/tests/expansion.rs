//! Small inputs that expand: copies per attempt, per test and per reference, value counts of
//! JSON documents, zip central directories and quadratic passes. Each test bounds memory with
//! a counting allocator (live bytes above a ceiling fail the allocation) or bounds time.
//!
//! Limits are built from JSON so this file also compiles against builds without the newer
//! limit fields (unknown fields are ignored), which shows the tests failing there.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fastproof_ingest::{IngestBatch, IngestError, IngestOptions, Limits, NullSink, ingest_path};
use tempfile::TempDir;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// Live heap above this fails the allocation (the process aborts instead of swapping).
const CEILING: usize = 1 << 30;

fn grow(n: usize) -> bool {
    let now = LIVE.fetch_add(n, Ordering::Relaxed) + n;
    if now > CEILING {
        LIVE.fetch_sub(n, Ordering::Relaxed);
        return false;
    }
    PEAK.fetch_max(now, Ordering::Relaxed);
    true
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !grow(layout.size()) {
            return std::ptr::null_mut();
        }
        let p = unsafe { System.alloc(layout) };
        if p.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() && !grow(new_size - layout.size()) {
            return std::ptr::null_mut();
        }
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if p.is_null() {
            if new_size > layout.size() {
                LIVE.fetch_sub(new_size - layout.size(), Ordering::Relaxed);
            }
        } else if new_size < layout.size() {
            LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
        }
        p
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Tests of this file run one at a time so the peak belongs to one ingest.
static SERIAL: Mutex<()> = Mutex::new(());

const MIB: usize = 1 << 20;

struct Run {
    result: Result<IngestBatch, IngestError>,
    peak: usize,
    took: Duration,
}

fn ingest(path: &Path, limits: Limits) -> Run {
    let o = IngestOptions {
        limits,
        ..IngestOptions::default()
    };
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let t = Instant::now();
    let result = ingest_path(path, &o, &mut NullSink::default());
    let took = t.elapsed();
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);
    Run { result, peak, took }
}

fn limits(v: serde_json::Value) -> Limits {
    serde_json::from_value(v).unwrap()
}

fn defaults() -> Limits {
    Limits::default()
}

fn limit_of(run: &Run) -> &'static str {
    match &run.result {
        Ok(b) => panic!(
            "expected a limit error, got {} results (peak {} MiB)",
            b.results.len(),
            run.peak / MIB
        ),
        Err(e) => e
            .limit_name()
            .unwrap_or_else(|| panic!("not a limit error: {e}")),
    }
}

fn write(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let p = dir.join(name);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&p, data).unwrap();
    p
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn junit_case(properties: &str, outcomes: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><testsuites><testsuite name="s"><testcase classname="pkg.C" name="t"><properties>{properties}</properties>{outcomes}</testcase></testsuite></testsuites>"#
    )
}

fn big_properties(n: usize, value_bytes: usize) -> String {
    (0..n)
        .map(|i| {
            format!(
                r#"<property name="p{i}" value="{}"/>"#,
                "v".repeat(value_bytes)
            )
        })
        .collect()
}

/// 1 MiB of labels and 2000 failure elements: 2 GiB of copies without the attempt limit.
#[test]
fn junit_attempts_per_test_are_capped() {
    let _g = lock();
    let t = TempDir::new().unwrap();
    let xml = junit_case(&big_properties(400, 2600), &"<failure/>".repeat(2000));
    let p = write(t.path(), "junit.xml", xml.as_bytes());
    let run = ingest(&p, defaults());
    assert_eq!(limit_of(&run), "max_attempts_per_result");
    assert!(run.peak < 64 * MIB, "peak {} MiB", run.peak / MIB);
}

/// Within the attempt limit, every copy is charged to the upload byte budget.
#[test]
fn junit_attempt_copies_are_charged_to_the_budget() {
    let _g = lock();
    let t = TempDir::new().unwrap();
    let xml = junit_case(&big_properties(300, 3400), &"<failure/>".repeat(90));
    let p = write(t.path(), "junit.xml", xml.as_bytes());
    let run = ingest(
        &p,
        limits(serde_json::json!({"max_total_uncompressed_bytes": 32 * MIB})),
    );
    assert_eq!(limit_of(&run), "max_total_uncompressed_bytes");
    assert!(run.peak < 64 * MIB, "peak {} MiB", run.peak / MIB);
}

#[test]
fn junit_labels_and_attachment_markers_per_result_are_capped() {
    let _g = lock();
    let t = TempDir::new().unwrap();
    let xml = junit_case(&big_properties(600, 1), "");
    let p = write(t.path(), "junit.xml", xml.as_bytes());
    assert_eq!(limit_of(&ingest(&p, defaults())), "max_labels_per_result");

    let markers: String = (0..5000)
        .map(|i| format!("[[ATTACHMENT|f{i}.png]]\n"))
        .collect();
    let xml = format!(
        r#"<testsuite name="s"><testcase classname="c" name="t"><system-out>{markers}</system-out></testcase></testsuite>"#
    );
    let p = write(t.path(), "junit.xml", xml.as_bytes());
    assert_eq!(
        limit_of(&ingest(&p, defaults())),
        "max_attachments_per_result"
    );
}

#[test]
fn long_names_are_truncated_with_a_warning() {
    let _g = lock();
    let t = TempDir::new().unwrap();
    let name = "n".repeat(MIB);
    let xml = format!(
        r#"<testsuite name="s"><testcase classname="{c}" name="{name}"><failure/><failure/></testcase></testsuite>"#,
        c = "c".repeat(100_000)
    );
    let p = write(t.path(), "junit.xml", xml.as_bytes());
    let b = ingest(&p, defaults()).result.unwrap();
    assert_eq!(b.results.len(), 2);
    for r in &b.results {
        assert!(r.name.len() <= 4096, "name {}", r.name.len());
        assert!(r.full_name.len() <= 4096, "full name {}", r.full_name.len());
        assert!(r.identity.full_name.len() <= 4096);
        assert!(r.identity.display_name.len() <= 4096);
    }
    assert!(
        b.warnings
            .iter()
            .any(|w| w.message.contains("max_name_bytes")),
        "{:?}",
        b.warnings
    );

    let d = t.path().join("allure");
    write(
        &d,
        "a-result.json",
        format!(
            r#"{{"uuid":"a","name":"{n}","fullName":"{n}","status":"passed","start":1,"stop":2}}"#,
            n = "x".repeat(100_000)
        )
        .as_bytes(),
    );
    let b = ingest(&d, defaults()).result.unwrap();
    assert!(b.results[0].name.len() <= 4096);
    assert!(b.results[0].full_name.len() <= 4096);
}

fn ctrf(tests: &str) -> String {
    format!(
        r#"{{"reportFormat":"CTRF","specVersion":"0.0.0","results":{{"tool":{{"name":"jest"}},"summary":{{"tests":1}},"tests":[{tests}]}}}}"#
    )
}

/// 10 000 steps and 5000 retries: 50 million step copies without the attempt limit.
#[test]
fn ctrf_retries_are_capped() {
    let _g = lock();
    let t = TempDir::new().unwrap();
    let steps = vec![r#"{"name":"s","status":"passed"}"#; 10_000].join(",");
    let retries = vec![r#"{"status":"failed"}"#; 5000].join(",");
    let doc = ctrf(&format!(
        r#"{{"name":"t","status":"passed","duration":1,"steps":[{steps}],"retryAttempts":[{retries}]}}"#
    ));
    let p = write(t.path(), "ctrf-report.json", doc.as_bytes());
    let run = ingest(&p, defaults());
    assert_eq!(limit_of(&run), "max_attempts_per_result");
    assert!(run.peak < 64 * MIB, "peak {} MiB", run.peak / MIB);
}

#[test]
fn ctrf_retry_copies_keep_steps_out_and_are_charged() {
    let _g = lock();
    let t = TempDir::new().unwrap();
    let steps = vec![r#"{"name":"s","status":"passed"}"#; 1000].join(",");
    let tags: String = (0..400)
        .map(|i| format!("\"{i}{}\"", "t".repeat(2600)))
        .collect::<Vec<_>>()
        .join(",");
    let retries = vec![r#"{"status":"failed"}"#; 60].join(",");
    let doc = ctrf(&format!(
        r#"{{"name":"t","status":"passed","duration":1,"tags":[{tags}],"steps":[{steps}],"retryAttempts":[{retries}]}}"#
    ));
    let p = write(t.path(), "ctrf-report.json", doc.as_bytes());
    let run = ingest(
        &p,
        limits(serde_json::json!({"max_total_uncompressed_bytes": 32 * MIB})),
    );
    assert_eq!(limit_of(&run), "max_total_uncompressed_bytes");

    let doc = ctrf(&format!(
        r#"{{"name":"t","status":"passed","duration":1,"steps":[{steps}],"retryAttempts":[{}]}}"#,
        [r#"{"status":"failed"}"#; 3].join(",")
    ));
    let p = write(t.path(), "ctrf-report.json", doc.as_bytes());
    let b = ingest(&p, defaults()).result.unwrap();
    let steps: Vec<usize> = b.results.iter().map(|r| r.steps.len()).collect();
    assert_eq!(steps, vec![0, 0, 0, 1000]);

    let tags = vec!["\"x\""; 600].join(",");
    let doc = ctrf(&format!(
        r#"{{"name":"t","status":"passed","tags":[{tags}]}}"#
    ));
    let p = write(t.path(), "ctrf-report.json", doc.as_bytes());
    assert_eq!(limit_of(&ingest(&p, defaults())), "max_labels_per_result");
}

fn playwright(specs: &str) -> String {
    format!(
        r#"{{"config":{{"rootDir":"/w","version":"1.56.1"}},"suites":[{{"title":"a.spec.ts","file":"a.spec.ts","specs":[{specs}],"suites":[]}}],"errors":[],"stats":{{"startTime":"2026-11-01T10:00:00.000Z","duration":1}}}}"#
    )
}

/// 1 MiB of spec tags shared by 3000 tests: 3 GiB of label copies, charged to the budget.
#[test]
fn playwright_spec_copies_are_charged_to_the_budget() {
    let _g = lock();
    let t = TempDir::new().unwrap();
    let tags: String = (0..400)
        .map(|i| format!("\"@{i}{}\"", "t".repeat(2600)))
        .collect::<Vec<_>>()
        .join(",");
    let test = r#"{"projectName":"chromium","results":[{"status":"passed","retry":0}]}"#;
    let tests = vec![test; 3000].join(",");
    let doc = playwright(&format!(
        r#"{{"title":"t","file":"a.spec.ts","line":1,"id":"x","tags":[{tags}],"tests":[{tests}]}}"#
    ));
    let p = write(t.path(), "results.json", doc.as_bytes());
    let run = ingest(
        &p,
        limits(serde_json::json!({"max_total_uncompressed_bytes": 64 * MIB})),
    );
    assert_eq!(limit_of(&run), "max_total_uncompressed_bytes");
    assert!(run.peak < 128 * MIB, "peak {} MiB", run.peak / MIB);
}

#[test]
fn playwright_results_per_test_and_inline_bodies_are_capped() {
    let _g = lock();
    let t = TempDir::new().unwrap();
    let results = vec![r#"{"status":"failed"}"#; 1000].join(",");
    let doc = playwright(&format!(
        r#"{{"title":"t","file":"a.spec.ts","tests":[{{"projectName":"p","results":[{results}]}}]}}"#
    ));
    let p = write(t.path(), "results.json", doc.as_bytes());
    assert_eq!(limit_of(&ingest(&p, defaults())), "max_attempts_per_result");

    let body = "QUFB".repeat(25_000);
    let atts = (0..20)
        .map(|i| format!(r#"{{"name":"a{i}","contentType":"text/plain","body":"{body}"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let doc = playwright(&format!(
        r#"{{"title":"t","file":"a.spec.ts","tests":[{{"projectName":"p","results":[{{"status":"passed","attachments":[{atts}]}}]}}]}}"#
    ));
    let p = write(t.path(), "results.json", doc.as_bytes());
    assert!(ingest(&p, defaults()).result.is_ok());
    assert_eq!(
        limit_of(&ingest(
            &p,
            limits(serde_json::json!({"max_inline_attachment_bytes": MIB}))
        )),
        "max_inline_attachment_bytes"
    );
}

/// 5.5 million values in 11 MB: hundreds of MiB as `serde_json::Value`.
#[test]
fn json_values_are_counted_before_parsing() {
    let _g = lock();
    let t = TempDir::new().unwrap();
    let mut doc = String::from(
        r#"{"reportFormat":"CTRF","results":{"tool":{"name":"t"},"tests":[],"extra":["#,
    );
    doc.push_str(&vec!["0"; 5_500_000].join(","));
    doc.push_str("]}}");
    let p = write(t.path(), "ctrf.json", doc.as_bytes());
    drop(doc);
    let run = ingest(&p, defaults());
    assert_eq!(limit_of(&run), "max_json_values");
    assert!(run.peak < 64 * MIB, "peak {} MiB", run.peak / MIB);
}

#[test]
fn attachment_references_are_capped_per_result_and_per_upload() {
    let _g = lock();
    let t = TempDir::new().unwrap();
    let d = t.path().join("one");
    let atts = (0..1001)
        .map(|i| format!(r#"{{"name":"a","source":"x{i}-attachment.txt"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    write(
        &d,
        "a-result.json",
        format!(r#"{{"uuid":"a","name":"t","status":"passed","attachments":[{atts}]}}"#).as_bytes(),
    );
    assert_eq!(
        limit_of(&ingest(&d, defaults())),
        "max_attachments_per_result"
    );

    let d = t.path().join("many");
    write(&d, "a-attachment.txt", b"x");
    let atts = [r#"{"name":"a","source":"a-attachment.txt"}"#; 5].join(",");
    for i in 0..3 {
        write(
            &d,
            &format!("{i}-result.json"),
            format!(r#"{{"uuid":"{i}","name":"t{i}","status":"passed","attachments":[{atts}]}}"#)
                .as_bytes(),
        );
    }
    assert!(ingest(&d, defaults()).result.is_ok());
    assert_eq!(
        limit_of(&ingest(
            &d,
            limits(serde_json::json!({"max_attachments": 10}))
        )),
        "max_attachments"
    );
}

fn zip_with_entries(path: &Path, n: usize) {
    let f = std::fs::File::create(path).unwrap();
    let mut z = zip::ZipWriter::new(std::io::BufWriter::new(f));
    let o =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for i in 0..n {
        z.start_file(format!("d/{i}-attachment.txt"), o).unwrap();
    }
    z.start_file("a-result.json", o).unwrap();
    z.write_all(br#"{"uuid":"a","name":"t","status":"passed","start":1,"stop":2}"#)
        .unwrap();
    z.finish().unwrap().flush().unwrap();
}

/// 60 000 central directory entries are rejected from the end record, before the zip crate
/// builds them.
#[test]
fn zip_entry_count_is_checked_before_the_central_directory_is_read() {
    let _g = lock();
    let t = TempDir::new().unwrap();
    let p = t.path().join("u.zip");
    zip_with_entries(&p, 60_000);
    let run = ingest(&p, limits(serde_json::json!({"max_archive_entries": 100})));
    assert_eq!(limit_of(&run), "max_archive_entries");
    assert!(run.peak < 4 * MIB, "peak {} KiB", run.peak / 1024);
}

fn eocd_offset(bytes: &[u8]) -> usize {
    (0..=bytes.len() - 22)
        .rev()
        .find(|&i| &bytes[i..i + 4] == b"PK\x05\x06")
        .unwrap()
}

#[test]
fn zip64_declared_entry_count_is_rejected() {
    let _g = lock();
    let t = TempDir::new().unwrap();
    let p = t.path().join("u.zip");
    zip_with_entries(&p, 1);
    let bytes = std::fs::read(&p).unwrap();
    let eocd = eocd_offset(&bytes);
    let cd_size = u32::from_le_bytes(bytes[eocd + 12..eocd + 16].try_into().unwrap()) as u64;
    let cd_offset = u32::from_le_bytes(bytes[eocd + 16..eocd + 20].try_into().unwrap()) as u64;
    let mut out = bytes[..eocd].to_vec();
    let eocd64_at = out.len() as u64;
    let declared: u64 = 1 << 40;
    out.extend_from_slice(b"PK\x06\x06");
    out.extend_from_slice(&44u64.to_le_bytes());
    out.extend_from_slice(&45u16.to_le_bytes());
    out.extend_from_slice(&45u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&declared.to_le_bytes());
    out.extend_from_slice(&declared.to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(b"PK\x06\x07");
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&eocd64_at.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    out.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    out.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    std::fs::write(&p, out).unwrap();
    assert_eq!(limit_of(&ingest(&p, defaults())), "max_archive_entries");
}

/// The end record understates the directory size; reading the directory is still capped.
#[test]
fn zip_central_directory_read_is_capped() {
    let _g = lock();
    let t = TempDir::new().unwrap();
    let p = t.path().join("u.zip");
    zip_with_entries(&p, 4000);
    assert!(ingest(&p, defaults()).result.is_ok());
    let small = limits(serde_json::json!({"max_zip_directory_bytes": 1024}));
    assert_eq!(
        limit_of(&ingest(&p, small.clone())),
        "max_zip_directory_bytes"
    );
    let mut bytes = std::fs::read(&p).unwrap();
    let eocd = eocd_offset(&bytes);
    bytes[eocd + 12..eocd + 16].copy_from_slice(&100u32.to_le_bytes());
    std::fs::write(&p, bytes).unwrap();
    let run = ingest(&p, small);
    assert_eq!(
        run.result.as_ref().err().and_then(IngestError::limit_name),
        Some("max_zip_directory_bytes"),
        "{:?}",
        run.result.as_ref().err()
    );
}

/// 10 000 property blocks of 16 distinct keys each: the launch attributes were deduplicated
/// by a linear scan per key (10^10 comparisons).
#[test]
fn suite_properties_are_merged_in_linear_time() {
    let _g = lock();
    let t = TempDir::new().unwrap();
    let mut xml = String::from(r#"<testsuites><testsuite name="s">"#);
    for b in 0..10_000 {
        xml.push_str("<properties>");
        for k in 0..16 {
            xml.push_str(&format!(r#"<property name="k{b}_{k}" value="v"/>"#));
        }
        xml.push_str("</properties>");
    }
    xml.push_str(r#"<testcase classname="c" name="t"/></testsuite></testsuites>"#);
    let p = write(t.path(), "junit.xml", xml.as_bytes());
    let run = ingest(&p, defaults());
    let b = run.result.as_ref().unwrap();
    assert_eq!(b.launch_meta.attributes.len(), 160_000);
    assert!(run.took < Duration::from_secs(20), "took {:?}", run.took);
}
