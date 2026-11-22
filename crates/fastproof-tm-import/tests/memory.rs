//! Peak memory of parses (L2, M4). Its own test binary: the global allocator counts the bytes
//! of every thread, so the tests here run one after another.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use fastproof_tm_import::{Format, ImportError, ParseOptions, parse, rich};

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

/// L2: a 20 MiB Qase JSON array of numbers fails on `maxImportRows` with memory proportional
/// to the file (it was about 13 times the file).
#[test]
fn qase_json_memory_is_proportional_to_the_file() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut doc = Vec::with_capacity(20 * MIB + 16);
    doc.extend_from_slice(b"{\"cases\":[0");
    while doc.len() < 20 * MIB {
        doc.extend_from_slice(b",0");
    }
    doc.extend_from_slice(b"]}");
    let (r, grown) = peak_of(|| {
        parse(
            Format::QaseJson,
            &doc[..],
            &ParseOptions::default(),
            &mut |_| true,
        )
    });
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
    assert!(
        grown < 3 * doc.len() + 8 * MIB,
        "{} MiB for a {} MiB file",
        grown / MIB,
        doc.len() / MIB
    );
}

/// L2: a valid Qase export still parses through the bounded reader.
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
            if let fastproof_tm_import::Item::Case(c) = i {
                names.push((c.name.clone(), c.folder.clone()));
            }
            true
        },
    )
    .unwrap();
    assert_eq!(
        names,
        vec![
            ("A".to_string(), vec!["Root".to_string()]),
            ("B".to_string(), vec!["Root".to_string(), "Sub".to_string()]),
            ("C".to_string(), vec![]),
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
