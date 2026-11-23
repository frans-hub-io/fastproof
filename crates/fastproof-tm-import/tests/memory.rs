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
                Item::FieldDefs(_) => {}
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
    let big = format!("\"{}\"", "x".repeat(300_000));
    let strings = vec![big; 30].join(",");
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
                    Item::FieldDefs(_) => {}
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
