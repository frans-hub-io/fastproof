//! Random corruptions of real and synthetic videos: preparing must never panic, must stop
//! within its deadline, and anything it accepts must parse as a seekable file.

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use common::*;
use fastproof_media::{Format, Limits, Outcome, webm};

#[test]
fn corrupted_inputs_never_panic_or_hang() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = rng(0xF022);
    let mut bases = vec![
        std::fs::read(recording()).unwrap(),
        std::fs::read(media_recorder()).unwrap(),
    ];
    for i in 0..4 {
        bases.push(Synth::random(&mut common::rng(i)).bytes());
        bases.push(
            Mp4Synth {
                second_mdat_after_moov: i % 2 == 0,
                co64: i == 3,
                ..Mp4Synth::default()
            }
            .bytes(&mut common::rng(i))
            .0,
        );
    }
    let iterations = std::env::var("FASTPROOF_MEDIA_FUZZ_ITERATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1200usize);
    let (mut prepared, mut failed, mut other) = (0, 0, 0);
    let mut slowest = Duration::ZERO;
    for i in 0..iterations {
        let base = &bases[i % bases.len()];
        let mut v = base.clone();
        for _ in 0..rand::Rng::random_range(&mut rng, 1..4) {
            v = corrupt(&mut rng, v);
        }
        let input = write(dir.path(), "in", &v);
        let out = dir.path().join("out");
        let limits = Limits {
            max_memory_bytes: 4 << 20,
            max_header_bytes: 1 << 20,
            deadline: Some(Instant::now() + Duration::from_secs(5)),
            ..Limits::default()
        };
        let started = Instant::now();
        let r = catch_unwind(AssertUnwindSafe(|| {
            fastproof_media::prepare(&input, &out, &limits)
        }));
        let took = started.elapsed();
        slowest = slowest.max(took);
        assert!(took < Duration::from_secs(6), "iteration {i} took {took:?}");
        let r = r.unwrap_or_else(|_| {
            std::fs::write(dir.path().join("panic"), &v).unwrap();
            panic!("iteration {i} panicked")
        });
        match r {
            Ok(Outcome::Prepared { format, bytes }) => {
                prepared += 1;
                assert_eq!(std::fs::metadata(&out).unwrap().len(), bytes);
                if format == Format::Webm {
                    let s = webm::verify(&out, &Limits::default()).unwrap_or_else(|e| {
                        panic!("iteration {i}: accepted output does not verify: {e}")
                    });
                    assert!(s.cues_point_at_clusters());
                }
            }
            Ok(_) => {
                other += 1;
                assert!(!out.exists(), "iteration {i}");
            }
            Err(_) => {
                failed += 1;
                assert!(!out.exists(), "iteration {i}");
            }
        }
    }
    eprintln!("prepared {prepared}, failed {failed}, left alone {other}, slowest {slowest:?}");
    assert!(prepared > 0 && failed > 0);
}
