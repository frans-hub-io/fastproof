//! Synthetic throughput check (ignored by default):
//! `cargo test --release -p fastproof-ingest --test bench -- --ignored --nocapture`

use std::io::Write;
use std::time::Instant;

use fastproof_ingest::{IngestOptions, NullSink, ingest_path};

fn peak_rss_kib() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines()
        .find(|l| l.starts_with("VmHWM:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
}

fn step(depth: u32, i: u32) -> String {
    let children = if depth < 2 {
        (0..3)
            .map(|j| step(depth + 1, j))
            .collect::<Vec<_>>()
            .join(",")
    } else {
        String::new()
    };
    format!(
        r#"{{"name":"step {depth}.{i} with a realistic name","status":"passed","stage":"finished","start":1795249075967,"stop":1795249075999,"parameters":[{{"name":"k","value":"v{i}"}}],"steps":[{children}]}}"#
    )
}

#[test]
#[ignore]
fn ingest_5000_allure_results() {
    let n = 5000;
    let dir = tempfile::TempDir::new().unwrap();
    let steps = (0..2).map(|i| step(0, i)).collect::<Vec<_>>().join(",");
    let mut bytes = 0usize;
    for i in 0..n {
        let status = if i % 20 == 0 { "failed" } else { "passed" };
        let doc = format!(
            r#"{{"uuid":"r{i}","historyId":"h{i}","testCaseId":"tc{i}","name":"test {i}","fullName":"com.example.suite{m}.Test{i}#run","status":"{status}","statusDetails":{{"message":"expected: <1> but was: <2>","trace":"java.lang.AssertionError\n\tat com.example.Test.run(Test.java:{i})"}},"stage":"finished","start":1795249075967,"stop":1795249076967,"labels":[{{"name":"suite","value":"suite{m}"}},{{"name":"feature","value":"f{m}"}},{{"name":"severity","value":"normal"}}],"parameters":[{{"name":"browser","value":"chromium"}},{{"name":"user","value":"u{i}","mode":"masked"}}],"links":[{{"type":"issue","url":"https://issues.example.com/T-{i}"}}],"attachments":[{{"name":"log","source":"r{i}-attachment.txt","type":"text/plain"}}],"steps":[{steps}]}}"#,
            m = i % 50
        );
        bytes += doc.len();
        std::fs::write(dir.path().join(format!("r{i}-result.json")), doc).unwrap();
        std::fs::write(
            dir.path().join(format!("r{i}-attachment.txt")),
            format!("log line for {i}\n").repeat(20),
        )
        .unwrap();
        if i % 10 == 0 {
            let children: Vec<String> = (i..i + 10).map(|c| format!("\"r{c}\"")).collect();
            std::fs::write(
                dir.path().join(format!("c{i}-container.json")),
                format!(r#"{{"uuid":"c{i}","children":[{}],"befores":[{{"name":"setUp","status":"passed","start":1,"stop":2}}],"afters":[{{"name":"tearDown","status":"passed","start":3,"stop":4}}]}}"#, children.join(",")),
            )
            .unwrap();
        }
    }
    let rss_before = peak_rss_kib();
    let t = Instant::now();
    let b = ingest_path(
        dir.path(),
        &IngestOptions::default(),
        &mut NullSink::default(),
    )
    .unwrap();
    let dir_time = t.elapsed();
    assert_eq!(b.results.len(), n);
    let steps_total = b.counts.steps;

    let tgz = dir.path().join("../bench-upload.tar.gz");
    {
        let f = std::fs::File::create(&tgz).unwrap();
        let gz = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
        let mut tb = tar::Builder::new(gz);
        tb.append_dir_all("allure-results", dir.path()).unwrap();
        tb.into_inner().unwrap().finish().unwrap().flush().unwrap();
    }
    let t = Instant::now();
    let b2 = ingest_path(&tgz, &IngestOptions::default(), &mut NullSink::default()).unwrap();
    let tgz_time = t.elapsed();
    assert_eq!(b2.results.len(), n);
    let tgz_size = std::fs::metadata(&tgz).unwrap().len();
    std::fs::remove_file(&tgz).unwrap();
    println!(
        "results={n} steps={steps_total} json_bytes={bytes} attachments={} dir={:?} tar.gz({} bytes)={:?} peak_rss_before={:?}KiB peak_rss_after={:?}KiB",
        b.counts.attachments_stored,
        dir_time,
        tgz_size,
        tgz_time,
        rss_before,
        peak_rss_kib()
    );
}
