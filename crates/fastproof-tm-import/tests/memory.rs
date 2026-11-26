//! Peak memory of parses (L2, M4). Its own test binary: the global allocator counts the bytes
//! of every thread, so the tests here run one after another.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use fastproof_tm_import::{Format, ImportError, Item, ParseOptions, parse, rich};

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static SERIAL: Mutex<()> = Mutex::new(());

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let now = CURRENT.fetch_add(l.size(), Ordering::Relaxed) + l.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        CURRENT.fetch_sub(l.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            if new >= l.size() {
                let now = CURRENT.fetch_add(new - l.size(), Ordering::Relaxed) + new - l.size();
                PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                CURRENT.fetch_sub(l.size() - new, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Bytes allocated at the peak of `f` above what was allocated before it.
fn peak_of<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = CURRENT.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let out = f();
    (out, PEAK.load(Ordering::Relaxed).saturating_sub(base))
}

const MIB: usize = 1024 * 1024;

/// Parses a Qase JSON document, counting cases and row errors without keeping them; returns
/// the result, the counts and the peak growth of allocated bytes.
fn qase_counts(doc: &[u8]) -> ((Result<(), ImportError>, usize, usize), usize) {
    let mut cases = 0;
    let mut errors = 0;
    let (r, grown) = peak_of(|| {
        parse(Format::QaseJson, doc, &ParseOptions::default(), &mut |i| {
            match i {
                Item::Case(_) => cases += 1,
                Item::Error(_) => errors += 1,
                Item::FieldDefs(_) | Item::SharedStep(_) | Item::Requirement(_) => {}
            }
            true
        })
    });
    ((r, cases, errors), grown)
}

/// L2: a 100 MiB Qase JSON array of numbers fails on `maxImportRows` after reading what it
/// needs, with memory that does not grow with the file (it was about 13 times the file,
/// then about the file).
#[test]
fn qase_json_numbers_fail_fast_in_constant_memory() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut doc = b"{\"cases\":[0".to_vec();
    doc.extend_from_slice(&b",0".repeat(50 * MIB));
    doc.extend_from_slice(b"]}");
    let ((r, _, _), grown) = qase_counts(&doc);
    assert!(
        matches!(
            r,
            Err(ImportError::Limit {
                limit: "maxImportRows",
                ..
            })
        ),
        "{r:?}"
    );
    assert!(grown < 4 * MIB, "{} KiB", grown / 1024);
}

/// L2: a large valid export streams: memory stays the same whatever the number of cases.
#[test]
fn qase_json_large_export_streams_in_constant_memory() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let n = 20_000;
    let text = "Lorem ipsum dolor sit amet. ".repeat(40);
    let mut doc = br#"{"code": "PRJ", "suites": [{"id": 1, "title": "Root", "cases": ["#.to_vec();
    for i in 0..n {
        if i > 0 {
            doc.push(b',');
        }
        doc.extend_from_slice(
            format!(
                r#"{{"id": {i}, "title": "Case {i}", "description": "{text}", "severity": "normal",
                "tags": [{{"title": "t{}"}}], "steps": [{{"action": "Do {i}", "expected_result": "Done"}},
                {{"action": "Check", "expected_result": "{text}"}}]}}"#,
                i % 7
            )
            .as_bytes(),
        );
    }
    doc.extend_from_slice(b"]}]}");
    assert!(doc.len() > 40 * MIB, "{} MiB", doc.len() / MIB);
    let ((r, cases, errors), grown) = qase_counts(&doc);
    r.unwrap();
    assert_eq!((cases, errors), (n, 0));
    assert!(
        grown < 4 * MIB,
        "{} KiB for a {} MiB file",
        grown / 1024,
        doc.len() / MIB
    );
}

/// Keys the parser does not read are skipped without being kept.
#[test]
fn qase_json_skipped_values_take_no_memory() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut doc = b"{\"meta\":[0".to_vec();
    doc.extend_from_slice(&b",0".repeat(10 * MIB));
    doc.extend_from_slice(br#"], "cases": [{"id": 1, "title": "A"}]}"#);
    let ((r, cases, _), grown) = qase_counts(&doc);
    r.unwrap();
    assert_eq!(cases, 1);
    assert!(grown < 4 * MIB, "{} KiB", grown / 1024);
}

/// A case past its budget (bytes of JSON or memory of its values) is a row error, its rest
/// is skipped, and the next case is read.
#[test]
fn qase_json_oversized_case_is_a_row_error() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let big = format!("\"{}\"", "x".repeat(60_000));
    let strings = vec![big; 150].join(",");
    let zeros = format!("0{}", ",0".repeat(4_000_000));
    let doc = format!(
        r#"{{"cases": [{{"id": 1, "title": "Big", "x": [{strings}]}}, {{"id": 2, "title": "Many", "n": [{zeros}]}},
        {{"id": 3, "title": "After"}}]}}"#
    );
    let mut names = Vec::new();
    let mut errors = Vec::new();
    let (r, grown) = peak_of(|| {
        parse(
            Format::QaseJson,
            doc.as_bytes(),
            &ParseOptions::default(),
            &mut |i| {
                match i {
                    Item::Case(c) => names.push(c.name.clone()),
                    Item::Error(e) => errors.push((e.row, e.code)),
                    Item::FieldDefs(_) | Item::SharedStep(_) | Item::Requirement(_) => {}
                }
                true
            },
        )
    });
    r.unwrap();
    assert_eq!(names, vec!["After"]);
    assert_eq!(
        errors,
        vec![
            (1, "limit_reached".to_string()),
            (2, "limit_reached".to_string())
        ]
    );
    assert!(grown < 64 * MIB, "{} MiB", grown / MIB);
}

/// L2: a valid Qase export still parses: cases of a suite read before its title wait for it.
#[test]
fn qase_json_cases_and_suites_still_parse() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let doc = br#"{"code": "PRJ", "suites": [{"cases": [{"id": 1, "title": "A"}], "title": "Root",
        "suites": [{"title": "Sub", "cases": [{"id": 2, "title": "B"}]}]}], "cases": [{"id": 3, "title": "C"}]}"#;
    let mut names = Vec::new();
    parse(
        Format::QaseJson,
        &doc[..],
        &ParseOptions::default(),
        &mut |i| {
            if let Item::Case(c) = i {
                names.push((c.name.clone(), c.folder.clone(), c.keys[0].value.clone()));
            }
            true
        },
    )
    .unwrap();
    assert_eq!(
        names,
        vec![
            (
                "A".to_string(),
                vec!["Root".to_string()],
                "PRJ-1".to_string()
            ),
            (
                "B".to_string(),
                vec!["Root".to_string(), "Sub".to_string()],
                "PRJ-2".to_string()
            ),
            ("C".to_string(), vec![], "PRJ-3".to_string()),
        ]
    );
}

/// Q1: two flat suites that are each other's parent, with long titles, named by many cases:
/// every path is built once, ends at the cycle and holds names cut to the name limit, so the
/// memory does not grow with the titles times the cases (it was 64 titles per case).
#[test]
fn qase_json_suite_cycle_takes_little_memory() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let long = "t".repeat(60_000);
    let mut doc = format!(
        r#"{{"suites_flat":[{{"id":1,"title":"{long}","parent_id":2}},{{"id":2,"title":"{long}","parent_id":1}}],"cases":["#
    );
    for i in 0..20_000 {
        if i > 0 {
            doc.push(',');
        }
        doc.push_str(&format!(
            r#"{{"id":{i},"title":"c","suite_id":{}}}"#,
            1 + i % 2
        ));
    }
    doc.push_str("]}");
    let mut longest = 0;
    let (r, grown) = peak_of(|| {
        parse(
            Format::QaseJson,
            doc.as_bytes(),
            &ParseOptions::default(),
            &mut |i| {
                if let Item::Case(c) = i {
                    longest = longest.max(c.folder.iter().map(String::len).sum::<usize>());
                }
                true
            },
        )
    });
    r.unwrap();
    assert!(longest <= 2 * 501, "{longest}");
    assert!(grown < 4 * MIB, "grew {grown} bytes");

    let huge = "t".repeat(384 * 1024);
    let doc = format!(
        r#"{{"suites_flat":[{{"id":1,"title":"{huge}","parent_id":2}},{{"id":2,"title":"{huge}","parent_id":1}}],"cases":[{{"id":1,"title":"a","suite_id":1}}]}}"#
    );
    let ((r, cases, _), grown) = qase_counts(doc.as_bytes());
    assert!(
        matches!(
            r,
            Err(ImportError::Limit {
                limit: "maxImportCellBytes",
                ..
            })
        ),
        "{r:?}"
    );
    assert_eq!(cases, 0);
    assert!(grown < 4 * MIB, "grew {grown} bytes");
}

/// Q2: cases of a suite whose title never comes wait for it only up to
/// `MAX_DEFERRED_BYTES`; then the file fails, holding at most about that much.
#[test]
fn qase_json_deferred_cases_are_bounded() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let max = fastproof_tm_import::qase::MAX_DEFERRED_BYTES;
    let body = "d".repeat(1000);
    let mut doc = String::from(r#"{"suites":[{"cases":["#);
    let mut i = 0;
    while doc.len() < max + 2 * MIB {
        if i > 0 {
            doc.push(',');
        }
        doc.push_str(&format!(
            r#"{{"id":{i},"title":"c","description":"{body}"}}"#
        ));
        i += 1;
    }
    doc.push_str(r#"],"title":"late"}]}"#);
    let ((r, cases, _), grown) = qase_counts(doc.as_bytes());
    match r {
        Err(ImportError::Invalid(m)) => assert!(m.contains("title"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(cases, 0);
    assert!(grown < max + 16 * MIB, "grew {grown} bytes");
}

/// M4: nested lists of a 64 KiB cell stay linear in size and memory.
#[test]
fn html_lists_and_tables_stay_linear() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let cell = "<ul>".repeat(10_000) + &"<li>".repeat((64 * 1024 - 40_000) / 4);
    assert!(cell.len() <= 64 * 1024);
    let (md, grown) = peak_of(|| rich(&cell));
    assert!(md.len() <= 8 * cell.len() + 4096, "{} bytes", md.len());
    assert!(grown < 4 * MIB, "{} MiB", grown / MIB);
    let table = "<table><tr>".to_string() + &"<td>x".repeat(8000) + &"<tr>".repeat(6000);
    let (md, grown) = peak_of(|| rich(&table));
    assert!(md.len() <= 8 * table.len() + 4096, "{} bytes", md.len());
    assert!(grown < 4 * MIB, "{} MiB", grown / MIB);
}

/// Review H2: a 15 MiB NDJSON line of tiny values (`{}` steps, or an unknown field that the
/// tagged v2 record buffers) took 1.3 to 1.5 GiB while parsed. The values of a line are
/// bounded now: such a line is a row error in little memory, and the next line still imports.
#[test]
fn ndjson_lines_of_tiny_values_are_row_errors_in_bounded_memory() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for (v, ty) in [(1, ""), (2, "\"type\":\"case\",")] {
        for (body, item) in [("\"steps\":[", &b"{},"[..]), ("\"junk\":[", &b"0,"[..])] {
            let mut doc = format!(
                "{{\"format\":\"fastproof-cases\",\"version\":{v},\"project\":\"P\",\"exportedAt\":\"x\",\"fields\":[]}}\n{{{ty}\"name\":\"big\",{body}"
            )
            .into_bytes();
            while doc.len() < 15 * MIB {
                doc.extend_from_slice(item);
            }
            doc.extend_from_slice(b"{}]}\n");
            doc.extend_from_slice(format!("{{{ty}\"name\":\"After\"}}\n").as_bytes());
            let mut names = Vec::new();
            let mut errors = Vec::new();
            let (r, grown) = peak_of(|| {
                parse(
                    Format::FastproofNdjson,
                    &doc[..],
                    &ParseOptions::default(),
                    &mut |i| {
                        match i {
                            Item::Case(c) => names.push(c.name.clone()),
                            Item::Error(e) => errors.push(e.code),
                            _ => {}
                        }
                        true
                    },
                )
            });
            assert!(r.is_ok(), "{r:?}");
            assert_eq!(names, vec!["After"], "v{v} {body}");
            assert_eq!(errors, vec!["invalid_row"], "v{v} {body}");
            assert!(grown < 96 * MIB, "v{v} {body}: {} MiB", grown / MIB);
        }
    }
}

/// Review H1: Qase `params` with many group rows of distinct names (or long values next
/// to many values) built every row over every name before the limits were checked, which
/// took gigabytes and minutes. The limits are checked while reading now.
#[test]
fn qase_params_past_the_limits_are_refused_in_bounded_memory() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut rows = String::new();
    for r in 0..1000 {
        if r > 0 {
            rows.push(',');
        }
        rows.push('{');
        for k in 0..20 {
            if k > 0 {
                rows.push(',');
            }
            rows.push_str(&format!("\"n{r}_{k}\":\"v\""));
        }
        rows.push('}');
    }
    let wide = format!("{{\"group\":[{rows}]}}");
    let many: Vec<String> = (0..1000).map(|i| format!("\"{i}\"")).collect();
    let long: Vec<String> = (0..19)
        .map(|i| format!("\"a{i}\":[\"{}\"]", "x".repeat(60 * 1024)))
        .collect();
    let huge = format!(
        "{{\"single\":{{{},\"b\":[{}]}}}}",
        long.join(","),
        many.join(",")
    );
    for params in [wide, huge] {
        let doc = format!(
            "{{\"suites\":[{{\"title\":\"S\",\"cases\":[{{\"title\":\"P\",\"params\":{params}}},{{\"title\":\"After\"}}]}}]}}"
        );
        let ((r, cases, errors), grown) = qase_counts(doc.as_bytes());
        assert!(r.is_ok(), "{r:?}");
        assert_eq!((cases, errors), (1, 1));
        assert!(grown < 64 * MIB, "{} MiB", grown / MIB);
    }
}
