//! The memory cap holds for hostile inputs: a counting allocator measures the peak heap of a
//! preparation (this binary has a single test, so nothing else allocates meanwhile).

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use fastproof_media::ebml::*;
use fastproof_media::{Error, Limits, Outcome};

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

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
}

#[global_allocator]
static ALLOC: Counting = Counting;

const CAP: u64 = 2 << 20;
/// Fixed buffers outside the budget: the input reader (64 KiB), the output writer (256 KiB),
/// the copy buffer (64 KiB) and growth slack of the index vectors.
const FIXED: usize = 1 << 20;

fn peak_of(f: impl FnOnce() -> Result<Outcome, Error>) -> (usize, Result<Outcome, Error>) {
    let base = CURRENT.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let r = f();
    (PEAK.load(Ordering::Relaxed) - base, r)
}

fn hostile(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut header = Vec::new();
    let mut e = Vec::new();
    write_element(&mut e, DOC_TYPE, b"webm");
    write_element(&mut header, EBML, &e);
    for (name, id, size) in [
        ("cues-just-under", CUES, CAP - 1024),
        ("cues-over", CUES, CAP + 1),
        ("tracks-over", TRACKS, 300 << 20),
        ("info-huge", INFO, 1 << 40),
    ] {
        let mut v = header.clone();
        write_id(&mut v, SEGMENT);
        v.push(0xFF);
        write_id(&mut v, id);
        write_size8(&mut v, size);
        v.extend(std::iter::repeat_n(
            0u8,
            (size.min(CAP) as usize).min(3 << 20),
        ));
        out.push((name.to_string(), v));
    }
    let mut many = Synth::random(&mut rng(11));
    many.clusters = (0..200_000)
        .map(|i| {
            (
                i,
                vec![SynthBlock {
                    track: 1,
                    rel: 0,
                    key: true,
                    group: false,
                    duration: None,
                    payload: vec![],
                }],
            )
        })
        .collect();
    out.push(("many-clusters".into(), many.bytes()));
    let _ = dir;
    out
}

#[test]
fn peak_heap_stays_under_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let limits = Limits {
        max_memory_bytes: CAP,
        max_header_bytes: 1 << 20,
        ..Limits::default()
    };
    let mut inputs = hostile(dir.path());
    inputs.push(("recording".into(), std::fs::read(recording()).unwrap()));
    inputs.push((
        "mediarecorder".into(),
        std::fs::read(media_recorder()).unwrap(),
    ));
    let mut r = rng(77);
    let base = std::fs::read(media_recorder()).unwrap();
    for i in 0..200 {
        inputs.push((format!("corrupt{i}"), corrupt(&mut r, base.clone())));
    }
    let mut worst = 0;
    for (name, bytes) in inputs {
        let p = write(dir.path(), "in", &bytes);
        drop(bytes);
        let out = dir.path().join("out");
        let (peak, res) = peak_of(|| fastproof_media::prepare(&p, &out, &limits));
        worst = worst.max(peak);
        assert!(
            peak <= CAP as usize + FIXED,
            "{name}: peak {peak} bytes, result {res:?}"
        );
        if name == "many-clusters" {
            assert!(matches!(res, Err(Error::Limit(_))), "{res:?}");
        }
        if name.ends_with("over") || name == "info-huge" {
            assert!(
                res.is_err() || matches!(res, Ok(Outcome::Unsupported(_))),
                "{name}: {res:?}"
            );
        }
    }
    eprintln!("worst peak {worst} bytes");
}
