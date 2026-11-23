//! MP4 faststart on synthetic files: every chunk offset of the copy must point at the same
//! chunk bytes as in the original.

mod common;

use common::*;
use fastproof_media::{Error, Format, Limits, Outcome};

/// The input, its chunks with their offsets, the result and the copy.
type Run = (
    Vec<u8>,
    Vec<(u64, Vec<u8>)>,
    Result<Outcome, Error>,
    Vec<u8>,
);

fn run(m: &Mp4Synth, seed: u64) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let (bytes, chunks) = m.bytes(&mut rng(seed));
    for (o, c) in &chunks {
        assert_eq!(
            &bytes[*o as usize..*o as usize + c.len()],
            c.as_slice(),
            "builder"
        );
    }
    let input = write(dir.path(), "in.mp4", &bytes);
    let out = dir.path().join("out.mp4");
    let r = fastproof_media::prepare(&input, &out, &Limits::default());
    let copy = std::fs::read(&out).unwrap_or_default();
    (bytes, chunks, r, copy)
}

fn assert_faststart(
    bytes: &[u8],
    chunks: &[(u64, Vec<u8>)],
    r: Result<Outcome, Error>,
    copy: &[u8],
) {
    let Ok(Outcome::Prepared { format, bytes: n }) = r else {
        panic!("{r:?}")
    };
    assert_eq!(format, Format::Mp4);
    assert_eq!(n as usize, copy.len());
    assert_eq!(copy.len(), bytes.len());
    let layout = mp4_layout(copy);
    let moov = layout.iter().position(|k| k == "moov").unwrap();
    let mdat = layout.iter().position(|k| k == "mdat").unwrap();
    assert!(moov < mdat, "{layout:?}");
    let offsets = mp4_offsets(copy);
    assert_eq!(offsets.len(), chunks.len());
    for (o, (_, c)) in offsets.iter().zip(chunks) {
        assert_eq!(&copy[*o as usize..*o as usize + c.len()], c.as_slice());
    }
}

#[test]
fn moov_at_the_end_moves_to_the_front() {
    for (i, m) in [
        Mp4Synth::default(),
        Mp4Synth {
            co64: true,
            ..Mp4Synth::default()
        },
        Mp4Synth {
            tracks: 3,
            chunks: 40,
            ..Mp4Synth::default()
        },
        Mp4Synth {
            free_before_moov: true,
            ..Mp4Synth::default()
        },
        Mp4Synth {
            second_mdat_after_moov: true,
            chunks: 10,
            ..Mp4Synth::default()
        },
        Mp4Synth {
            moov_size_zero: true,
            ..Mp4Synth::default()
        },
        Mp4Synth {
            extra: Some((*b"udta", vec![0u8; 12])),
            ..Mp4Synth::default()
        },
    ]
    .iter()
    .enumerate()
    {
        let (bytes, chunks, r, copy) = run(m, i as u64);
        assert_faststart(&bytes, &chunks, r, &copy);
    }
}

#[test]
fn files_left_alone() {
    let (_, _, r, copy) = run(
        &Mp4Synth {
            moov_first: true,
            ..Mp4Synth::default()
        },
        1,
    );
    assert_eq!(r.unwrap(), Outcome::AlreadySeekable(Format::Mp4));
    assert!(copy.is_empty());
    let (_, _, r, _) = run(
        &Mp4Synth {
            extra: Some((*b"mvex", vec![0u8; 8])),
            ..Mp4Synth::default()
        },
        2,
    );
    assert_eq!(r.unwrap(), Outcome::AlreadySeekable(Format::Mp4));
    let mut meta = vec![0u8; 4];
    meta.extend_from_slice(&16u32.to_be_bytes());
    meta.extend_from_slice(b"iloc");
    meta.extend_from_slice(&[0u8; 8]);
    let (_, _, r, _) = run(
        &Mp4Synth {
            extra: Some((*b"meta", meta)),
            ..Mp4Synth::default()
        },
        3,
    );
    assert!(matches!(r, Ok(Outcome::Unsupported(_))), "{r:?}");
}

#[test]
fn offsets_into_moov_and_item_locations_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (mut v, _) = Mp4Synth::default().bytes(&mut rng(9));
    let moov = v.windows(4).rposition(|w| w == b"moov").unwrap() - 4;
    let stco = v.windows(4).position(|w| w == b"stco").unwrap();
    let last = stco + 12 + 4 * 4;
    v[last..last + 4].copy_from_slice(&(moov as u32 + 8).to_be_bytes());
    let p = write(dir.path(), "into-moov.mp4", &v);
    let r = fastproof_media::prepare(&p, &dir.path().join("o"), &Limits::default());
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");

    let mut iloc = vec![0u8; 4];
    iloc.extend_from_slice(&16u32.to_be_bytes());
    iloc.extend_from_slice(b"iloc");
    iloc.extend_from_slice(&[0u8; 8]);
    let mut meta = 28u32.to_be_bytes().to_vec();
    meta.extend_from_slice(b"meta");
    meta.extend_from_slice(&iloc);
    meta.extend_from_slice(&[0u8; 4]);
    for (i, udta) in [meta.clone(), meta[..meta.len() - 4].to_vec()]
        .into_iter()
        .enumerate()
    {
        let (_, _, r, _) = run(
            &Mp4Synth {
                extra: Some((*b"udta", udta)),
                ..Mp4Synth::default()
            },
            10 + i as u64,
        );
        assert!(matches!(r, Ok(Outcome::Unsupported(_))), "{r:?}");
    }
}

#[test]
fn malformed_mp4() {
    let dir = tempfile::tempdir().unwrap();
    let (bytes, _) = Mp4Synth::default().bytes(&mut rng(4));
    let p = write(dir.path(), "cut.mp4", &bytes[..bytes.len() - 3]);
    let r = fastproof_media::prepare(&p, &dir.path().join("o"), &Limits::default());
    assert!(matches!(r, Err(Error::Truncated)), "{r:?}");

    let mut v = bytes.clone();
    let stco = v.windows(4).position(|w| w == b"stco").unwrap();
    v[stco + 8..stco + 12].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
    let p = write(dir.path(), "count.mp4", &v);
    let r = fastproof_media::prepare(&p, &dir.path().join("o"), &Limits::default());
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");

    let mut v = bytes.clone();
    let stco = v.windows(4).position(|w| w == b"stco").unwrap();
    v[stco + 12..stco + 16].copy_from_slice(&0x7FFF_FFFFu32.to_be_bytes());
    let p = write(dir.path(), "outside.mp4", &v);
    let r = fastproof_media::prepare(&p, &dir.path().join("o"), &Limits::default());
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");

    let tight = Limits {
        max_memory_bytes: 64,
        ..Limits::default()
    };
    let p = write(dir.path(), "big-moov.mp4", &bytes);
    let r = fastproof_media::prepare(&p, &dir.path().join("o"), &tight);
    assert!(matches!(r, Err(Error::Limit(_))), "{r:?}");
    assert!(!dir.path().join("o").exists());
}
