//! WebM preparation: real Chromium recordings, synthetic round trips checked against an
//! independent demuxer, and malformed inputs.

mod common;

use common::*;
use fastproof_media::webm::{self, scan_file};
use fastproof_media::{Error, Format, Limits, Outcome};

#[test]
fn playwright_recording_already_seeks() {
    let dir = tempfile::tempdir().unwrap();
    let (s, _) = blocks(&recording());
    assert!(s.duration.is_some());
    assert!(!s.cues.is_empty());
    assert!(s.cues_after_clusters && s.seek_head_lists_cues);
    assert_eq!(s.needs_index(), None);
    let (r, out) = prepare(&recording(), dir.path());
    assert_eq!(r, Outcome::AlreadySeekable(Format::Webm));
    assert!(!out.exists());
}

#[test]
fn media_recorder_clip_gets_duration_and_cues() {
    let dir = tempfile::tempdir().unwrap();
    let input = media_recorder();
    let (s, before) = blocks(&input);
    assert_eq!(s.duration, None);
    assert!(s.cues.is_empty());
    assert!(s.segment_unknown);
    assert!(s.clusters.iter().all(|c| c.unknown_size));
    assert_eq!(s.needs_index(), Some("the segment has an unknown size"));

    let (r, out) = prepare(&input, dir.path());
    let Outcome::Prepared { format, bytes } = r else {
        panic!("{r:?}")
    };
    assert_eq!(format, Format::Webm);
    assert_eq!(bytes, std::fs::metadata(&out).unwrap().len());
    let (o, after) = blocks(&out);
    assert_eq!(o.needs_index(), None);
    assert!(same_blocks(&before, &after), "blocks differ");
    let end = s.end_time.unwrap();
    assert!(end > 5_000, "{end}");
    assert_eq!(o.duration, Some(end as f64));
    assert_eq!(o.end_time, Some(end));
    assert!(!o.cues_after_clusters);
    assert!(o.cues_point_at_clusters());
    let keyed = o.clusters.iter().filter(|c| c.first_key.is_some()).count();
    assert_eq!(o.cues.len(), keyed);
    for c in &o.cues {
        let cl = o
            .clusters
            .iter()
            .find(|k| k.start - o.segment_data == c.position)
            .unwrap();
        assert_eq!(Some(c.time as i64), cl.first_key);
    }

    let (d_in, scale, f_in) = demux(&input);
    let (d_out, scale_out, f_out) = demux(&out);
    assert_eq!(d_in, None);
    assert_eq!(scale, scale_out);
    assert_eq!(d_out, Some(end as f64));
    assert_eq!(f_in, f_out, "the independent demuxer sees other frames");

    let f = std::fs::File::open(&out).unwrap();
    let mut m = matroska_demuxer::MatroskaFile::open(f).unwrap();
    let target = (end as f64 * 0.8) as u64;
    m.seek(target).unwrap();
    let mut frame = matroska_demuxer::Frame::default();
    assert!(m.next_frame(&mut frame).unwrap());
    let got = frame.timestamp as i64 - target as i64;
    assert!(
        got.abs() <= (1_000_000_000 / scale) as i64,
        "seek to 80 % landed {got} units away"
    );

    let (again, _) = prepare(&out, &dir.path().join("x"));
    assert_eq!(again, Outcome::AlreadySeekable(Format::Webm));
}

#[test]
fn recording_without_cues_and_duration_is_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let input = std::fs::read(recording()).unwrap();
    let s = scan_file(&recording(), &Limits::default(), &mut |_| {}).unwrap();
    let mut stripped = input.clone();
    let info = find(&input, &fastproof_media::ebml::INFO.to_be_bytes());
    let d = info
        + find(
            &input[info..],
            &fastproof_media::ebml::DURATION.to_be_bytes()[2..],
        );
    stripped[d] = fastproof_media::ebml::VOID as u8;
    let cues = rfind(&input, &fastproof_media::ebml::CUES.to_be_bytes());
    stripped.truncate(cues);
    let p = write(dir.path(), "stripped.webm", &stripped);
    let t = scan_file(&p, &Limits::default(), &mut |_| {}).unwrap();
    assert_eq!(t.duration, None);
    assert!(t.cues.is_empty());
    assert_eq!(t.end_time, s.end_time);
    let (r, out) = prepare(&p, dir.path());
    assert!(matches!(r, Outcome::Prepared { .. }), "{r:?}");
    let (_, a) = blocks(&recording());
    let (o, b) = blocks(&out);
    assert!(same_blocks(&a, &b));
    assert_eq!(o.duration, s.end_time.map(|t| t as f64));
    assert_eq!(o.cues.len(), s.cues.len());
    assert_eq!(
        o.cues.iter().map(|c| c.time).collect::<Vec<_>>(),
        s.cues.iter().map(|c| c.time).collect::<Vec<_>>()
    );
    assert_eq!(demux(&recording()).2, demux(&out).2);
}

fn rfind(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len())
        .rposition(|w| w == needle)
        .unwrap()
}

fn find(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).position(|w| w == needle).unwrap()
}

#[test]
fn synthetic_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = rng(0x5eed_0001);
    let mut prepared = 0;
    for i in 0..300 {
        let synth = Synth::random(&mut rng);
        let p = write(dir.path(), &format!("in{i}.webm"), &synth.bytes());
        let (s, before) = blocks(&p);
        assert_eq!(s.end_time, Some(synth.end_time()), "case {i}: {synth:?}");
        let out = dir.path().join(format!("out{i}.webm"));
        let r = fastproof_media::prepare(&p, &out, &Limits::default()).unwrap();
        let has_key = synth
            .clusters
            .iter()
            .any(|(_, b)| b.iter().any(|b| b.key && b.track == 1));
        match r {
            Outcome::Prepared { .. } => {
                prepared += 1;
                let (o, after) = blocks(&out);
                assert_eq!(o.needs_index(), None, "case {i}");
                assert!(same_blocks(&before, &after), "case {i}");
                assert_eq!(o.duration, Some(synth.end_time() as f64), "case {i}");
                assert!(o.cues_point_at_clusters(), "case {i}");
                let want: Vec<u64> = synth
                    .clusters
                    .iter()
                    .filter_map(|(ts, b)| {
                        b.iter()
                            .find(|b| b.key && b.track == 1)
                            .map(|b| (*ts as i64 + b.rel as i64) as u64)
                    })
                    .collect();
                let mut want = want;
                want.sort();
                assert_eq!(
                    o.cues.iter().map(|c| c.time).collect::<Vec<_>>(),
                    want,
                    "case {i}"
                );
                let (_, _, f_in) = demux(&p);
                let (d, _, f_out) = demux(&out);
                assert_eq!(f_in, f_out, "case {i}");
                assert_eq!(d, Some(synth.end_time() as f64), "case {i}");
            }
            Outcome::Unsupported(m) => {
                assert!(!has_key, "case {i}: {m}");
                assert!(!out.exists());
            }
            Outcome::AlreadySeekable(_) => panic!("case {i}: synthetic files have no cues"),
        }
    }
    assert!(prepared > 200, "{prepared}");
}

#[test]
fn files_with_cues_and_duration_are_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = rng(7);
    for i in 0..50 {
        let mut s = Synth::random(&mut rng);
        s.unknown_segment = false;
        s.unknown_clusters = false;
        s.duration = Some(1000.0);
        s.cues = true;
        for (_, b) in &mut s.clusters {
            b[0].key = true;
            b[0].track = 1;
        }
        let p = write(dir.path(), &format!("c{i}.webm"), &s.bytes());
        let (r, _) = prepare(&p, dir.path());
        assert_eq!(r, Outcome::AlreadySeekable(Format::Webm), "case {i}");
    }
}

#[test]
fn truncated_recording_keeps_complete_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let input = std::fs::read(media_recorder()).unwrap();
    let (_, all) = blocks(&media_recorder());
    for cut in [input.len() * 3 / 4, input.len() / 2, input.len() - 7] {
        let p = write(dir.path(), "cut.webm", &input[..cut]);
        let (s, before) = blocks(&p);
        assert!(s.truncated);
        assert!(before.len() < all.len());
        assert!(same_blocks(&before, &all[..before.len()]));
        let (r, out) = prepare(&p, dir.path());
        assert!(matches!(r, Outcome::Prepared { .. }), "{r:?}");
        let (o, after) = blocks(&out);
        assert!(same_blocks(&before, &after));
        assert_eq!(o.needs_index(), None);
    }
}

#[test]
fn malformed_files() {
    let dir = tempfile::tempdir().unwrap();
    let limits = Limits::default();
    let run = |name: &str, bytes: &[u8]| {
        let p = write(dir.path(), name, bytes);
        fastproof_media::prepare(&p, &dir.path().join("out"), &limits)
    };
    assert!(matches!(run("empty", b""), Ok(Outcome::Unsupported(_))));
    assert!(matches!(
        run("text", b"hello world, not a video"),
        Ok(Outcome::Unsupported(_))
    ));
    assert!(matches!(
        run("magic", &[0x1A, 0x45, 0xDF, 0xA3]),
        Err(Error::Truncated)
    ));

    let mut doc = Synth::random(&mut rng(1)).bytes();
    let at = find(&doc, b"webm");
    doc[at..at + 4].copy_from_slice(b"wxyz");
    assert!(matches!(run("doctype", &doc), Err(Error::Invalid(_))));

    let mut s = Synth::random(&mut rng(2));
    s.unknown_segment = false;
    let mut v = s.bytes();
    let info = find(&v, &fastproof_media::ebml::INFO.to_be_bytes());
    v[info + 4] = 0xFF;
    assert!(
        matches!(run("unknown-info", &v), Err(Error::Invalid(_))),
        "unknown-size Info"
    );

    let mut v = s.bytes();
    let info = find(&v, &fastproof_media::ebml::INFO.to_be_bytes());
    v.splice(
        info + 4..info + 5,
        [0x01, 0x00, 0x00, 0x00, 0x7F, 0xFF, 0xFF, 0xF0],
    );
    let r = run("huge-info", &v);
    assert!(!matches!(r, Ok(Outcome::Prepared { .. })), "{r:?}");

    let p = write(dir.path(), "header", &Synth::random(&mut rng(3)).bytes());
    let tight = Limits {
        max_header_bytes: 10,
        ..Limits::default()
    };
    assert!(matches!(
        fastproof_media::prepare(&p, &dir.path().join("o"), &tight),
        Err(Error::Limit(_))
    ));

    let mut s = Synth::random(&mut rng(4));
    s.clusters.truncate(1);
    s.clusters[0].1.iter_mut().for_each(|b| {
        b.key = false;
        b.group = false;
    });
    s.unknown_segment = true;
    let r = run("no-keyframe", &s.bytes());
    assert!(matches!(r, Ok(Outcome::Unsupported(_))), "{r:?}");

    let small = Limits {
        max_input_bytes: 10,
        ..Limits::default()
    };
    let p = write(dir.path(), "limit", &Synth::random(&mut rng(5)).bytes());
    assert!(matches!(
        fastproof_media::prepare(&p, &dir.path().join("o"), &small),
        Err(Error::Limit(_))
    ));
    let late = Limits {
        deadline: Some(std::time::Instant::now()),
        ..Limits::default()
    };
    let p = media_recorder();
    assert!(matches!(
        fastproof_media::prepare(&p, &dir.path().join("o"), &late),
        Err(Error::Timeout)
    ));
    assert!(!dir.path().join("o").exists());
}

#[test]
fn block_time_overflow_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    for ts in [i64::MAX as u64, 1 << 63] {
        let mut s = Synth::random(&mut rng(12));
        s.clusters = vec![(
            ts,
            vec![SynthBlock {
                track: 1,
                rel: if ts == i64::MAX as u64 { 1 } else { -1 },
                key: true,
                group: false,
                duration: None,
                payload: vec![1, 2],
            }],
        )];
        let p = write(dir.path(), "overflow.webm", &s.bytes());
        let r = fastproof_media::prepare(&p, &dir.path().join("o"), &Limits::default());
        assert!(matches!(r, Err(Error::Invalid(_))), "{ts}: {r:?}");
    }
}

#[test]
fn many_tiny_elements_hit_the_element_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Synth::random(&mut rng(9));
    s.clusters = vec![(
        0,
        (0..5000)
            .map(|i| SynthBlock {
                track: 1,
                rel: (i % 30000) as i16,
                key: i == 0,
                group: false,
                duration: None,
                payload: vec![],
            })
            .collect(),
    )];
    let p = write(dir.path(), "many.webm", &s.bytes());
    let limits = Limits {
        max_elements: 1000,
        ..Limits::default()
    };
    assert!(matches!(
        fastproof_media::prepare(&p, &dir.path().join("o"), &limits),
        Err(Error::Limit(_))
    ));
    let limits = Limits {
        max_memory_bytes: 1000,
        ..Limits::default()
    };
    let mut many = Synth::random(&mut rng(10));
    many.clusters = (0..100)
        .map(|i| {
            (
                i * 10,
                vec![SynthBlock {
                    track: 1,
                    rel: 0,
                    key: true,
                    group: false,
                    duration: None,
                    payload: vec![1],
                }],
            )
        })
        .collect();
    let p = write(dir.path(), "clusters.webm", &many.bytes());
    assert!(matches!(
        fastproof_media::prepare(&p, &dir.path().join("o"), &limits),
        Err(Error::Limit(_))
    ));
}

#[test]
fn verify_reports_a_good_file() {
    let dir = tempfile::tempdir().unwrap();
    let (_, out) = prepare(&media_recorder(), dir.path());
    let s = webm::verify(&out, &Limits::default()).unwrap();
    assert!(!s.cues.is_empty());
    assert!(webm::verify(&media_recorder(), &Limits::default()).is_err());
}
