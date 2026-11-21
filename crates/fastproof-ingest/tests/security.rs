//! Limits and hostile inputs.

use std::io::Write;
use std::path::{Path, PathBuf};

use fastproof_ingest::{IngestError, IngestOptions, Limits, MemorySink, NullSink, ingest_path};
use fastproof_model::{SourceFormat, Status};
use tempfile::TempDir;

fn opts(limits: Limits) -> IngestOptions {
    IngestOptions {
        limits,
        ..IngestOptions::default()
    }
}

fn run(path: &Path, o: &IngestOptions) -> Result<fastproof_ingest::IngestBatch, IngestError> {
    ingest_path(path, o, &mut NullSink::default())
}

fn limit_of(r: Result<fastproof_ingest::IngestBatch, IngestError>) -> &'static str {
    match r {
        Ok(_) => panic!("expected a limit error"),
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

fn result_json(uuid: &str, extra: &str) -> String {
    format!(
        r#"{{"uuid":"{uuid}","name":"t {uuid}","fullName":"pkg.Test#t{uuid}","status":"passed","stage":"finished","start":1,"stop":2{extra}}}"#
    )
}

fn zip_of(path: &Path, entries: &[(&str, &[u8])]) {
    let f = std::fs::File::create(path).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let o = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in entries {
        z.start_file(*name, o).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap();
}

fn raw_tar_entry(name: &[u8], kind: tar::EntryType, data: &[u8], link: Option<&[u8]>) -> Vec<u8> {
    let mut h = tar::Header::new_ustar();
    h.as_old_mut().name[..name.len()].copy_from_slice(name);
    if let Some(l) = link {
        h.as_old_mut().linkname[..l.len()].copy_from_slice(l);
    }
    h.set_entry_type(kind);
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    let mut out = h.as_bytes().to_vec();
    out.extend_from_slice(data);
    let pad = (512 - data.len() % 512) % 512;
    out.extend(std::iter::repeat_n(0u8, pad));
    out
}

fn finish_tar(mut body: Vec<u8>) -> Vec<u8> {
    body.extend(std::iter::repeat_n(0u8, 1024));
    body
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

#[test]
fn zip_bomb_is_stopped_by_the_byte_budget() {
    let t = TempDir::new().unwrap();
    let zeros = vec![0u8; 8 << 20];
    let p = t.path().join("bomb.zip");
    zip_of(&p, &[("a-result.json", &zeros), ("b-result.json", &zeros)]);
    assert!(std::fs::metadata(&p).unwrap().len() < 100_000);
    let o = opts(Limits {
        max_total_uncompressed_bytes: 10 << 20,
        ..Limits::default()
    });
    assert_eq!(limit_of(run(&p, &o)), "max_total_uncompressed_bytes");
}

#[test]
fn zip_entry_with_lying_size_header_is_bounded_while_streaming() {
    let t = TempDir::new().unwrap();
    let zeros = vec![b' '; 4 << 20];
    let p = t.path().join("lie.zip");
    zip_of(&p, &[("a-result.json", &zeros)]);
    let mut bytes = std::fs::read(&p).unwrap();
    // Rewrite the declared uncompressed size (local header and central directory) to 16.
    for (sig, off) in [
        (&[0x50u8, 0x4b, 0x03, 0x04][..], 22usize),
        (&[0x50, 0x4b, 0x01, 0x02][..], 24),
    ] {
        let pos = bytes.windows(4).position(|w| w == sig).unwrap();
        bytes[pos + off..pos + off + 4].copy_from_slice(&16u32.to_le_bytes());
    }
    std::fs::write(&p, &bytes).unwrap();
    let o = opts(Limits {
        max_entry_bytes: 1 << 20,
        max_document_bytes: 8 << 20,
        ..Limits::default()
    });
    let r = run(&p, &o);
    match r {
        Err(e) => assert!(
            e.limit_name() == Some("max_entry_bytes")
                || matches!(e, IngestError::Io(_) | IngestError::Archive(_)),
            "{e}"
        ),
        Ok(b) => assert!(
            b.results.is_empty(),
            "never more than the declared bytes are trusted"
        ),
    }
}

#[test]
fn tar_gz_bomb_is_charged_even_for_unread_entries() {
    let t = TempDir::new().unwrap();
    let big = vec![0u8; 16 << 20];
    let tar = finish_tar(raw_tar_entry(
        b"big.bin",
        tar::EntryType::Regular,
        &big,
        None,
    ));
    let p = write(t.path(), "bomb.tar.gz", &gzip(&tar));
    let o = opts(Limits {
        max_total_uncompressed_bytes: 4 << 20,
        ..Limits::default()
    });
    assert_eq!(limit_of(run(&p, &o)), "max_total_uncompressed_bytes");
}

#[test]
fn archive_path_traversal_and_links_are_rejected() {
    let t = TempDir::new().unwrap();
    let ok = result_json("1", "");
    for (name, kind, link, reason) in [
        (
            &b"../evil-result.json"[..],
            tar::EntryType::Regular,
            None,
            "parent directory component",
        ),
        (
            b"a/../../evil-result.json",
            tar::EntryType::Regular,
            None,
            "parent directory component",
        ),
        (
            b"/etc/evil-result.json",
            tar::EntryType::Regular,
            None,
            "absolute path",
        ),
        (
            b"link",
            tar::EntryType::Symlink,
            Some(&b"/etc/passwd"[..]),
            "link entry",
        ),
        (
            b"hard",
            tar::EntryType::Link,
            Some(&b"x-result.json"[..]),
            "link entry",
        ),
    ] {
        let mut body = raw_tar_entry(
            b"x-result.json",
            tar::EntryType::Regular,
            ok.as_bytes(),
            None,
        );
        body.extend(raw_tar_entry(name, kind, b"", link));
        let p = write(t.path(), "t.tar", &finish_tar(body));
        match run(&p, &IngestOptions::default()) {
            Err(IngestError::UnsafeEntry { reason: r, .. }) => assert_eq!(r, reason),
            other => panic!(
                "{:?}: {:?}",
                String::from_utf8_lossy(name),
                other.map(|b| b.counts)
            ),
        }
    }
    let p = t.path().join("z.zip");
    zip_of(
        &p,
        &[
            ("ok-result.json", ok.as_bytes()),
            ("../../evil-result.json", ok.as_bytes()),
        ],
    );
    assert!(matches!(
        run(&p, &IngestOptions::default()),
        Err(IngestError::UnsafeEntry { .. })
    ));

    let p = t.path().join("s.zip");
    {
        let f = std::fs::File::create(&p).unwrap();
        let mut z = zip::ZipWriter::new(f);
        z.add_symlink(
            "link",
            "/etc/passwd",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        z.finish().unwrap();
    }
    assert!(matches!(
        run(&p, &IngestOptions::default()),
        Err(IngestError::UnsafeEntry {
            reason: "symbolic link",
            ..
        })
    ));
}

#[test]
fn tar_long_names_are_bounded() {
    let t = TempDir::new().unwrap();
    let long = vec![b'a'; 100_000];
    let mut body = raw_tar_entry(b"././@LongLink", tar::EntryType::GNULongName, &long, None);
    body.extend(raw_tar_entry(b"x", tar::EntryType::Regular, b"{}", None));
    let p = write(t.path(), "long.tar", &finish_tar(body));
    assert_eq!(
        limit_of(run(&p, &IngestOptions::default())),
        "max_path_bytes"
    );
}

#[test]
fn tar_gz_with_results_and_attachments_and_pax_names() {
    let t = TempDir::new().unwrap();
    let name = format!("{}/r-result.json", "d".repeat(150));
    let r = result_json(
        "1",
        r#","attachments":[{"name":"log","source":"a-attachment.txt","type":"text/plain"}]"#,
    );
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_size(r.len() as u64);
    h.set_mode(0o644);
    b.append_data(&mut h, &name, r.as_bytes()).unwrap();
    let mut h = tar::Header::new_gnu();
    h.set_size(5);
    h.set_mode(0o644);
    b.append_data(
        &mut h,
        format!("{}/a-attachment.txt", "d".repeat(150)),
        &b"hello"[..],
    )
    .unwrap();
    let data = b.into_inner().unwrap();
    let p = write(t.path(), "u.tgz", &gzip(&data));
    let mut sink = MemorySink::default();
    let batch = ingest_path(&p, &IngestOptions::default(), &mut sink).unwrap();
    assert_eq!(batch.upload_kind, fastproof_ingest::UploadKind::TarGz);
    assert_eq!(batch.results.len(), 1);
    let a = &batch.results[0].attachments[0];
    assert_eq!(sink.get(a.id.as_ref().unwrap()).unwrap().bytes, b"hello");
}

#[test]
fn xml_billion_laughs_and_xxe_do_not_expand() {
    let t = TempDir::new().unwrap();
    let xml = r#"<?xml version="1.0"?>
<!DOCTYPE lolz [
 <!ENTITY lol "lol">
 <!ENTITY lol2 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">
 <!ENTITY lol3 "&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;">
 <!ENTITY lol9 "&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;">
 <!ENTITY xxe SYSTEM "file:///etc/passwd">
]>
<testsuite name="s"><testcase classname="c" name="n &lol9;"><failure message="&xxe;">&lol9; &xxe; &amp;</failure></testcase></testsuite>"#;
    let p = write(t.path(), "lol.xml", xml.as_bytes());
    let b = run(&p, &IngestOptions::default()).unwrap();
    assert_eq!(b.results.len(), 1);
    let r = &b.results[0];
    assert_eq!(r.name, "n &lol9;");
    assert_eq!(r.status_details.message.as_deref(), Some("&xxe;"));
    assert_eq!(r.status_details.trace.as_deref(), Some("&lol9; &xxe; &"));
    assert!(!r.status_details.trace.as_deref().unwrap().contains("root:"));
}

#[test]
fn xml_depth_and_attribute_limits() {
    let t = TempDir::new().unwrap();
    let deep = format!(
        "<testsuites>{}{}</testsuites>",
        "<testsuite>".repeat(200),
        "</testsuite>".repeat(200)
    );
    let p = write(t.path(), "deep.xml", deep.as_bytes());
    assert_eq!(
        limit_of(run(&p, &IngestOptions::default())),
        "max_xml_depth"
    );
    let attrs: String = (0..200).map(|i| format!(" a{i}=\"x\"")).collect();
    let wide = format!("<testsuite><testcase name=\"t\"{attrs}/></testsuite>");
    let p = write(t.path(), "wide.xml", wide.as_bytes());
    assert_eq!(
        limit_of(run(&p, &IngestOptions::default())),
        "max_xml_attributes"
    );
}

#[test]
fn xml_tolerates_control_chars_ansi_and_bad_utf8() {
    let t = TempDir::new().unwrap();
    let mut xml = b"<testsuite name=\"s\"><testcase classname=\"c\" name=\"t\"><system-out>\x1b[31mred\x1b[0m \x01 \xff ok</system-out></testcase></testsuite>".to_vec();
    xml.extend_from_slice(b"\n");
    let p = write(t.path(), "ctl.xml", &xml);
    let b = run(&p, &IngestOptions::default()).unwrap();
    let out = &b.results[0].stdout.as_ref().unwrap().text;
    assert!(out.contains("\u{1b}[31mred"), "ANSI kept");
    assert!(out.contains('\u{FFFD}') && out.ends_with("ok"));
}

#[test]
fn json_depth_string_and_step_limits() {
    let t = TempDir::new().unwrap();
    let mut nested = String::from("{\"name\":\"s\"}");
    for _ in 0..300 {
        nested = format!("{{\"name\":\"s\",\"steps\":[{nested}]}}");
    }
    let d = t.path().join("deep");
    write(
        &d,
        "a-result.json",
        result_json("1", &format!(",\"steps\":[{nested}]")).as_bytes(),
    );
    assert_eq!(
        limit_of(run(&d, &IngestOptions::default())),
        "max_json_depth"
    );

    let s = t.path().join("str");
    let big = "x".repeat(2000);
    write(
        &s,
        "a-result.json",
        result_json("1", &format!(",\"description\":\"{big}\"")).as_bytes(),
    );
    let o = opts(Limits {
        max_string_bytes: 1000,
        ..Limits::default()
    });
    assert_eq!(limit_of(run(&s, &o)), "max_string_bytes");

    let w = t.path().join("wide");
    let steps: Vec<String> = (0..50)
        .map(|i| format!("{{\"name\":\"s{i}\",\"steps\":[{{\"name\":\"c\"}}]}}"))
        .collect();
    write(
        &w,
        "a-result.json",
        result_json("1", &format!(",\"steps\":[{}]", steps.join(","))).as_bytes(),
    );
    let o = opts(Limits {
        max_steps_per_result: 60,
        ..Limits::default()
    });
    assert_eq!(limit_of(run(&w, &o)), "max_steps_per_result");
    let o = opts(Limits {
        max_steps_per_result: 100,
        ..Limits::default()
    });
    assert_eq!(run(&w, &o).unwrap().counts.steps, 100);
}

#[test]
fn container_cannot_multiply_fixtures() {
    let t = TempDir::new().unwrap();
    let steps: Vec<String> = (0..40).map(|i| format!("{{\"name\":\"f{i}\"}}")).collect();
    let befores = format!("[{{\"name\":\"setup\",\"steps\":[{}]}}]", steps.join(","));
    let children: Vec<String> = (0..10_000).map(|_| "\"1\"".to_string()).collect();
    let d = t.path().join("dup");
    write(&d, "1-result.json", result_json("1", "").as_bytes());
    write(
        &d,
        "c-container.json",
        format!(
            "{{\"uuid\":\"c\",\"children\":[{}],\"befores\":{befores}}}",
            children.join(",")
        )
        .as_bytes(),
    );
    let b = run(&d, &IngestOptions::default()).unwrap();
    assert_eq!(
        b.results[0].fixtures.befores.len(),
        1,
        "duplicate children are attached once"
    );
    assert_eq!(b.containers[0].children.len(), 1);

    let m = t.path().join("many");
    write(&m, "1-result.json", result_json("1", "").as_bytes());
    for i in 0..300 {
        write(
            &m,
            &format!("c{i}-container.json"),
            format!("{{\"uuid\":\"c{i}\",\"children\":[\"1\"],\"befores\":{befores}}}").as_bytes(),
        );
    }
    assert_eq!(
        limit_of(run(&m, &IngestOptions::default())),
        "max_steps_per_result"
    );
    let o = opts(Limits {
        max_total_uncompressed_bytes: 600_000,
        ..Limits::default()
    });
    let budget_hit = run(&m, &o).unwrap_err().limit_name();
    assert!(matches!(
        budget_hit,
        Some("max_total_uncompressed_bytes") | Some("max_steps_per_result")
    ));
}

#[test]
fn count_limits() {
    let t = TempDir::new().unwrap();
    let d = t.path().join("many");
    for i in 0..20 {
        write(
            &d,
            &format!("{i}-result.json"),
            result_json(
                &i.to_string(),
                &format!(",\"attachments\":[{{\"name\":\"a\",\"source\":\"{i}-attachment.txt\"}}]"),
            )
            .as_bytes(),
        );
        write(&d, &format!("{i}-attachment.txt"), b"x");
    }
    let lim = |f: &dyn Fn(&mut Limits)| {
        let mut l = Limits::default();
        f(&mut l);
        limit_of(run(&d, &opts(l)))
    };
    assert_eq!(lim(&|l| l.max_results = 10), "max_results");
    assert_eq!(lim(&|l| l.max_attachments = 10), "max_attachments");
    assert_eq!(lim(&|l| l.max_archive_entries = 10), "max_archive_entries");
    assert_eq!(lim(&|l| l.max_document_bytes = 50), "max_document_bytes");
    let z = t.path().join("u.zip");
    zip_of(&z, &[("a-result.json", result_json("1", "").as_bytes())]);
    assert_eq!(
        limit_of(run(
            &z,
            &opts(Limits {
                max_upload_bytes: 10,
                ..Limits::default()
            })
        )),
        "max_upload_bytes"
    );
}

#[test]
fn lenient_allure_parsing() {
    let t = TempDir::new().unwrap();
    let d = t.path();
    write(d, "1-result.json", br#"{"uuid":"1","name":"x","fullName":"a#x","status":"failed","start":"5","stop":7.9,"unknownField":{"deep":[1,2]},"labels":[{"name":"ALLURE_ID","value":"77"},null,{"value":"no name"}],"parameters":[{"name":"n","value":3},{"name":"p","value":"v","mode":"weird"}],"descriptionHtml":"<p onclick='x()'>ok<script>alert(1)</script></p>","statusDetails":{"message":"m","flaky":true}}"#);
    write(d, "2-result.json", br#"{"uuid":"2","name":"running","fullName":"a#running","status":"passed","stage":"running","start":1}"#);
    write(
        d,
        "3-result.json",
        br#"{"uuid":"3","name":"nostop","fullName":"a#nostop","start":1}"#,
    );
    write(d, "4-result.json", b"{not json");
    write(d, ".allure-write-123.tmp", b"partial");
    write(d, "history/history.json", b"{}");
    write(d, "x-container.json", br#"{"uuid":"x","children":["1","missing"],"afters":[{"name":"teardown","status":"passed"}]}"#);
    write(d, "g-globals.json", br#"{"errors":[{"message":"setup crashed","timestamp":3}],"attachments":[{"name":"log","source":"g-attachment.log","type":"text/plain"}]}"#);
    write(d, "g-attachment.log", b"global log");
    write(d, "executor.json", br#"{"name":"GitLab","type":"gitlab","buildOrder":12,"buildName":"pipeline 12","buildUrl":"https://ci.example.com/p/12","reportName":"nightly"}"#);
    let b = run(d, &IngestOptions::default()).unwrap();
    assert_eq!(b.results.len(), 3);
    assert_eq!(b.warnings.len(), 1, "{:?}", b.warnings);
    assert!(b.warnings[0].file.as_deref() == Some("4-result.json"));
    assert_eq!(b.ignored.len(), 2);
    let r1 = b.results.iter().find(|r| r.name == "x").unwrap();
    assert_eq!(
        (r1.start, r1.stop, r1.status),
        (Some(5), Some(7), Status::Failed)
    );
    assert!(r1.status_details.flaky);
    assert_eq!(
        r1.parameters[0].value, None,
        "non-string values are missing values, as in Allure 3"
    );
    assert_eq!(r1.description.html.as_deref(), Some("<p>ok</p>"));
    assert_eq!(r1.identity.explicit_id.as_ref().unwrap().value, "77");
    assert_eq!(r1.fixtures.afters.len(), 1);
    let running = b.results.iter().find(|r| r.name == "running").unwrap();
    assert!(running.interrupted);
    let nostop = b.results.iter().find(|r| r.name == "nostop").unwrap();
    assert!(nostop.interrupted && nostop.status == Status::Unknown);
    assert_eq!(
        b.globals.errors[0].message.as_deref(),
        Some("setup crashed")
    );
    assert!(b.globals.attachments[0].id.is_some());
    assert_eq!(b.launch_meta.ci.provider.as_deref(), Some("gitlab"));
    assert_eq!(b.launch_meta.ci.build_order, Some(12));
    assert_eq!(b.launch_meta.name.as_deref(), Some("nightly"));
}

#[test]
fn ctrf_legacy_and_single_file_upload() {
    let t = TempDir::new().unwrap();
    let doc = br#"{"results":{"tool":{"name":"jest"},"summary":{"tests":2,"passed":1,"failed":1,"skipped":0,"pending":0,"other":0,"start":0,"stop":0},
      "tests":[{"name":"adds","status":"passed","duration":3,"suite":"math > add","filePath":"src/math.test.js"},
               {"name":"divides","status":"failed","duration":4,"message":"expected 1 to be 2","suite":"math > div","filePath":"src/math.test.js","retries":1,
                "retryAttempts":[{"status":"failed","message":"first"}]}]}}"#;
    let p = write(t.path(), "spooled-upload", doc);
    let o = IngestOptions {
        file_name: Some("ctrf-report.json".into()),
        ..IngestOptions::default()
    };
    let b = run(&p, &o).unwrap();
    assert_eq!(b.format, Some(SourceFormat::Ctrf));
    assert_eq!(b.results.len(), 3);
    assert_eq!(
        b.results[0].identity.full_name,
        "src/math.test.js \u{203a} math \u{203a} add \u{203a} adds"
    );
    assert_eq!(b.retry_groups().len(), 2);
    assert_eq!(b.results[2].status_details.expected.as_deref(), Some("2"));
}

#[test]
fn environment_key_and_redaction_options() {
    let t = TempDir::new().unwrap();
    write(
        t.path(),
        "1-result.json",
        result_json(
            "1",
            r#","parameters":[{"name":"token","value":"s3cret","mode":"masked"}]"#,
        )
        .as_bytes(),
    );
    let base = run(t.path(), &IngestOptions::default()).unwrap();
    let o = IngestOptions {
        environment_key: Some("staging".into()),
        redact_secret_parameters: false,
        ..IngestOptions::default()
    };
    let b = run(t.path(), &o).unwrap();
    let r = &b.results[0];
    assert_eq!(r.parameters[0].value.as_deref(), Some("s3cret"));
    assert_eq!(base.results[0].parameters[0].value, None);
    assert_eq!(
        r.identity.parameters_hash,
        base.results[0].identity.parameters_hash
    );
    assert_eq!(
        r.identity.series_key,
        format!(
            "{}.{}",
            base.results[0].identity.series_key,
            fastproof_model::identity::md5_hex("staging")
        )
    );
    assert_eq!(b.launch_meta.environment_key.as_deref(), Some("staging"));
}

#[test]
fn store_unreferenced_files() {
    let t = TempDir::new().unwrap();
    write(t.path(), "1-result.json", result_json("1", "").as_bytes());
    write(t.path(), "extra.png", b"png");
    let mut sink = MemorySink::default();
    let b = ingest_path(t.path(), &IngestOptions::default(), &mut sink).unwrap();
    assert_eq!(b.unreferenced.len(), 1);
    assert!(sink.items.is_empty());
    let o = IngestOptions {
        store_unreferenced: true,
        ..IngestOptions::default()
    };
    let b = ingest_path(t.path(), &o, &mut sink).unwrap();
    assert_eq!(b.attachments.len(), 1);
    assert!(!b.attachments[0].referenced);
}

#[test]
fn junit_nested_suites_properties_and_names() {
    let t = TempDir::new().unwrap();
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="all">
  <testsuite name="outer"><properties><property name="ci">yes</property></properties>
    <testsuite name="inner" hostname="build-7">
      <testcase classname=" com.example.Café " name="adds(int, int)[2]" time="0.5" timestamp="2026-01-01T00:00:00">
        <properties><property name="ALLURE_ID" value="42"/><property name="issue" value="https://issues.example.com/T-1"/></properties>
      </testcase>
      <testcase classname="c" name="t"><skipped message="not now"/></testcase>
    </testsuite>
  </testsuite>
</testsuites>"#;
    let p = write(t.path(), "nested.xml", xml.as_bytes());
    let b = run(&p, &IngestOptions::default()).unwrap();
    assert_eq!(b.results.len(), 2);
    let r = &b.results[0];
    assert_eq!(r.identity.full_name, "com.example.Caf\u{e9}#adds");
    assert_eq!(
        r.identity.test_case_hash,
        fastproof_model::identity::md5_hex("42")
    );
    assert_eq!(
        r.parameters
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        vec!["signature", "index"]
    );
    assert_eq!(r.start, Some(1_767_225_600_000));
    assert_eq!(
        (r.duration_ms, r.stop),
        (Some(500), Some(1_767_225_600_500))
    );
    assert!(
        r.labels
            .iter()
            .any(|l| l.name == "suite" && l.value == "inner")
    );
    assert!(
        r.labels
            .iter()
            .any(|l| l.name == "host" && l.value == "build-7")
    );
    assert_eq!(r.links[0].link_type.as_deref(), Some("issue"));
    assert_eq!(b.results[1].status, Status::Skipped);
    assert_eq!(b.launch_meta.attribute("ci"), Some("yes"));
    assert_eq!(b.launch_meta.name.as_deref(), Some("all"));
}
