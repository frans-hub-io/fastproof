//! Helpers of the media tests: the fixtures, a builder of synthetic WebM and MP4 files, and the
//! frames of a file as both this crate and an independent demuxer see them.

#![allow(dead_code)]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use fastproof_media::ebml::*;
use fastproof_media::webm::{self, Block};
use fastproof_media::{Limits, Outcome};
use rand::rngs::StdRng;
use rand::{Rng, RngCore};
use sha2::{Digest, Sha256};

pub fn data(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(rel)
}

/// The Playwright screen recording (ffmpeg muxer: Duration, Cues after the clusters, SeekHead).
pub fn recording() -> PathBuf {
    data("playwright/test-results/screen-screen-recording/video.webm")
}

/// The MediaRecorder clip (no Duration, no Cues, unknown sizes).
pub fn media_recorder() -> PathBuf {
    let dir = data(
        "playwright/test-results/mediarecorder-canvas-recorded-with-MediaRecorder/attachments",
    );
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "webm"))
        .unwrap()
}

/// Blocks of a file with the SHA-256 of each block element.
pub fn blocks(path: &Path) -> (webm::Scan, Vec<(Block, String)>) {
    let mut seen = Vec::new();
    let s = webm::scan_file(path, &Limits::default(), &mut |b| seen.push(b.clone())).unwrap();
    let mut f = File::open(path).unwrap();
    let out = seen
        .into_iter()
        .map(|b| {
            f.seek(SeekFrom::Start(b.element.start)).unwrap();
            let mut v = vec![0u8; (b.element.end - b.element.start) as usize];
            f.read_exact(&mut v).unwrap();
            let h = hex::encode(Sha256::digest(&v));
            (b, h)
        })
        .collect();
    (s, out)
}

/// Same blocks in the same order, the element bytes included (offsets may differ).
pub fn same_blocks(a: &[(Block, String)], b: &[(Block, String)]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|((x, hx), (y, hy))| {
            x.track == y.track
                && x.time == y.time
                && x.key == y.key
                && x.frames == y.frames
                && hx == hy
        })
}

/// Frames as the independent `matroska-demuxer` reads them: `(track, timestamp, keyframe, data hash)`.
/// A demuxed frame: track, timestamp, keyframe flag, hash of the data.
pub type Frame = (u64, u64, bool, String);

pub fn demux(path: &Path) -> (Option<f64>, u64, Vec<Frame>) {
    let f = File::open(path).unwrap();
    let mut m = matroska_demuxer::MatroskaFile::open(f).unwrap();
    let duration = m.info().duration();
    let scale = m.info().timestamp_scale().get();
    let mut frame = matroska_demuxer::Frame::default();
    let mut out = Vec::new();
    while m.next_frame(&mut frame).unwrap() {
        out.push((
            frame.track,
            frame.timestamp,
            frame.is_keyframe.unwrap_or(false),
            hex::encode(Sha256::digest(&frame.data)),
        ));
    }
    (duration, scale, out)
}

pub fn prepare(input: &Path, dir: &Path) -> (Outcome, PathBuf) {
    let out = dir.join("out.webm");
    let r = fastproof_media::prepare(input, &out, &Limits::default()).unwrap();
    (r, out)
}

/// A synthetic block.
#[derive(Clone, Debug)]
pub struct SynthBlock {
    pub track: u64,
    pub rel: i16,
    pub key: bool,
    pub group: bool,
    pub duration: Option<u64>,
    pub payload: Vec<u8>,
}

/// A synthetic WebM file.
#[derive(Clone, Debug)]
pub struct Synth {
    pub clusters: Vec<(u64, Vec<SynthBlock>)>,
    pub unknown_segment: bool,
    pub unknown_clusters: bool,
    pub duration: Option<f64>,
    pub cues: bool,
    pub audio: bool,
    pub default_duration: Option<u64>,
    pub tags: bool,
    pub void_after_info: bool,
}

impl Synth {
    pub fn random(rng: &mut StdRng) -> Self {
        let audio = rng.random_bool(0.3);
        let n = rng.random_range(1..12);
        let mut t = 0u64;
        let mut clusters = Vec::new();
        for _ in 0..n {
            let k = rng.random_range(1..20);
            let mut blocks = Vec::new();
            let mut rel = 0i16;
            for j in 0..k {
                let track = if audio && rng.random_bool(0.3) { 2 } else { 1 };
                let len = rng.random_range(0..200);
                let payload: Vec<u8> = (0..len).map(|_| rng.random()).collect();
                blocks.push(SynthBlock {
                    track,
                    rel,
                    key: (j == 0 && rng.random_bool(0.8)) || rng.random_bool(0.05),
                    group: rng.random_bool(0.3),
                    duration: rng.random_bool(0.2).then(|| rng.random_range(1..80)),
                    payload,
                });
                rel = rel.saturating_add(rng.random_range(0..60));
            }
            clusters.push((t, blocks));
            t += rel as u64 + rng.random_range(1..100);
        }
        Synth {
            clusters,
            unknown_segment: rng.random_bool(0.5),
            unknown_clusters: rng.random_bool(0.5),
            duration: rng
                .random_bool(0.3)
                .then(|| rng.random_range(1.0..10_000.0)),
            cues: false,
            audio,
            default_duration: rng.random_bool(0.5).then_some(40_000_000),
            tags: rng.random_bool(0.3),
            void_after_info: rng.random_bool(0.3),
        }
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut e = Vec::new();
        write_uint(&mut e, EBML_VERSION, 1);
        write_uint(&mut e, EBML_READ_VERSION, 1);
        write_element(&mut e, DOC_TYPE, b"webm");
        write_uint(&mut e, DOC_TYPE_VERSION, 4);
        write_uint(&mut e, DOC_TYPE_READ_VERSION, 2);
        write_element(&mut out, EBML, &e);

        let mut seg = Vec::new();
        let mut cue_slot = None;
        if self.cues {
            let mut s = Vec::new();
            write_element(&mut s, SEEK_ID, &CUES.to_be_bytes());
            write_uint8(&mut s, SEEK_POSITION, 0);
            let mut sh = Vec::new();
            write_element(&mut sh, SEEK, &s);
            write_element(&mut seg, SEEK_HEAD, &sh);
            cue_slot = Some(seg.len() - 8);
        }
        let mut info = Vec::new();
        write_uint(&mut info, TIMESTAMP_SCALE, 1_000_000);
        write_element(&mut info, 0x4D80, b"synth");
        write_element(&mut info, 0x5741, b"synth");
        if let Some(d) = self.duration {
            write_float(&mut info, DURATION, d);
        }
        write_element(&mut seg, INFO, &info);
        if self.void_after_info {
            write_element(&mut seg, VOID, &[0u8; 20]);
        }
        let mut tracks = Vec::new();
        let mut v = Vec::new();
        write_uint(&mut v, TRACK_NUMBER, 1);
        write_uint(&mut v, 0x73C5, 11);
        write_uint(&mut v, TRACK_TYPE, 1);
        write_element(&mut v, 0x86, b"V_VP8");
        if let Some(d) = self.default_duration {
            write_uint(&mut v, DEFAULT_DURATION, d);
        }
        write_element(&mut tracks, TRACK_ENTRY, &v);
        if self.audio {
            let mut a = Vec::new();
            write_uint(&mut a, TRACK_NUMBER, 2);
            write_uint(&mut a, 0x73C5, 22);
            write_uint(&mut a, TRACK_TYPE, 2);
            write_element(&mut a, 0x86, b"A_OPUS");
            write_element(&mut tracks, TRACK_ENTRY, &a);
        }
        write_element(&mut seg, TRACKS, &tracks);
        if self.tags {
            write_element(&mut seg, TAGS, &[0x73, 0x73, 0x80]);
        }
        let mut positions = Vec::new();
        for (ts, blocks) in &self.clusters {
            positions.push(seg.len() as u64);
            let mut c = Vec::new();
            write_uint(&mut c, TIMESTAMP, *ts);
            for b in blocks {
                let mut body = Vec::new();
                write_size(&mut body, b.track);
                body.extend_from_slice(&b.rel.to_be_bytes());
                if b.group {
                    body.push(0);
                    body.extend_from_slice(&b.payload);
                    let mut g = Vec::new();
                    write_element(&mut g, BLOCK, &body);
                    if let Some(d) = b.duration {
                        write_uint(&mut g, BLOCK_DURATION, d);
                    }
                    if !b.key {
                        write_element(&mut g, REFERENCE_BLOCK, &[0xFF]);
                    }
                    write_element(&mut c, BLOCK_GROUP, &g);
                } else {
                    body.push(if b.key { 0x80 } else { 0 });
                    body.extend_from_slice(&b.payload);
                    write_element(&mut c, SIMPLE_BLOCK, &body);
                }
            }
            write_id(&mut seg, CLUSTER);
            if self.unknown_clusters {
                seg.push(0xFF);
            } else {
                write_size(&mut seg, c.len() as u64);
            }
            seg.extend_from_slice(&c);
        }
        if self.cues {
            let mut cues = Vec::new();
            for (i, (ts, blocks)) in self.clusters.iter().enumerate() {
                if let Some(b) = blocks.iter().find(|b| b.key && b.track == 1) {
                    let mut tp = Vec::new();
                    write_uint(&mut tp, CUE_TRACK, 1);
                    write_uint(&mut tp, CUE_CLUSTER_POSITION, positions[i]);
                    let mut cp = Vec::new();
                    write_uint(&mut cp, CUE_TIME, (*ts as i64 + b.rel as i64) as u64);
                    write_element(&mut cp, CUE_TRACK_POSITIONS, &tp);
                    write_element(&mut cues, CUE_POINT, &cp);
                }
            }
            if let Some(at) = cue_slot {
                let pos = seg.len() as u64;
                seg[at..at + 8].copy_from_slice(&pos.to_be_bytes());
            }
            write_element(&mut seg, CUES, &cues);
        }
        write_id(&mut out, SEGMENT);
        if self.unknown_segment {
            out.push(0xFF);
        } else {
            write_size(&mut out, seg.len() as u64);
        }
        out.extend_from_slice(&seg);
        out
    }

    /// The block end the output's Duration must equal.
    pub fn end_time(&self) -> i64 {
        let mut end = 0i64;
        for (ts, blocks) in &self.clusters {
            for b in blocks {
                let t = *ts as i64 + b.rel as i64;
                let span = match b.duration.filter(|_| b.group) {
                    Some(d) => d as i64,
                    None if b.track == 1 => {
                        self.default_duration.map_or(0, |d| (d / 1_000_000) as i64)
                    }
                    None => 0,
                };
                end = end.max(t + span);
            }
        }
        end
    }
}

/// Applies one random corruption.
pub fn corrupt(rng: &mut StdRng, mut v: Vec<u8>) -> Vec<u8> {
    if v.is_empty() {
        return v;
    }
    match rng.random_range(0..6) {
        0 => {
            let n = rng.random_range(1..8);
            for _ in 0..n {
                let i = rng.random_range(0..v.len());
                v[i] ^= 1 << rng.random_range(0..8);
            }
        }
        1 => {
            let at = rng.random_range(0..v.len());
            v.truncate(at);
        }
        2 => {
            let at = rng.random_range(0..v.len());
            let n = rng.random_range(1..64);
            let junk: Vec<u8> = (0..n).map(|_| rng.random()).collect();
            v.splice(at..at, junk);
        }
        3 => {
            let at = rng.random_range(0..v.len());
            let n = rng.random_range(1..64).min(v.len() - at);
            v.drain(at..at + n);
        }
        4 => {
            let at = rng.random_range(0..v.len());
            let width = rng.random_range(1..=8usize).min(v.len() - at);
            let huge = [0x01u8, 0x7F, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE];
            v[at..at + width].copy_from_slice(&huge[..width]);
        }
        _ => {
            let at = rng.random_range(0..v.len());
            v[at] = 0xFF;
        }
    }
    v
}

pub fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

pub fn rng(seed: u64) -> StdRng {
    rand::SeedableRng::seed_from_u64(seed)
}

pub fn random_bytes(rng: &mut StdRng, n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    rng.fill_bytes(&mut v);
    v
}

fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(payload);
    v
}

/// Layout of a synthetic MP4 file.
#[derive(Clone, Debug)]
pub struct Mp4Synth {
    pub moov_first: bool,
    pub co64: bool,
    pub tracks: usize,
    pub chunks: usize,
    pub free_before_moov: bool,
    pub second_mdat_after_moov: bool,
    pub moov_size_zero: bool,
    pub extra: Option<([u8; 4], Vec<u8>)>,
}

impl Default for Mp4Synth {
    fn default() -> Self {
        Self {
            moov_first: false,
            co64: false,
            tracks: 1,
            chunks: 5,
            free_before_moov: false,
            second_mdat_after_moov: false,
            moov_size_zero: false,
            extra: None,
        }
    }
}

impl Mp4Synth {
    fn moov(&self, offsets: &[Vec<u64>]) -> Vec<u8> {
        let mut moov = mp4_box(b"mvhd", &[0u8; 100]);
        for offs in offsets {
            let mut t = Vec::new();
            t.extend_from_slice(&0u32.to_be_bytes());
            t.extend_from_slice(&(offs.len() as u32).to_be_bytes());
            for o in offs {
                if self.co64 {
                    t.extend_from_slice(&o.to_be_bytes());
                } else {
                    t.extend_from_slice(&(*o as u32).to_be_bytes());
                }
            }
            let table = mp4_box(if self.co64 { b"co64" } else { b"stco" }, &t);
            let mut stbl = mp4_box(b"stsd", &[0u8; 16]);
            stbl.extend(table);
            let stbl = mp4_box(b"stbl", &stbl);
            let minf = mp4_box(b"minf", &stbl);
            let mut mdia = mp4_box(b"mdhd", &[0u8; 24]);
            mdia.extend(minf);
            let mdia = mp4_box(b"mdia", &mdia);
            let mut trak = mp4_box(b"tkhd", &[0u8; 84]);
            trak.extend(mdia);
            moov.extend(mp4_box(b"trak", &trak));
        }
        if let Some((k, p)) = &self.extra {
            moov.extend(mp4_box(k, p));
        }
        let mut m = mp4_box(b"moov", &moov);
        if self.moov_size_zero {
            m[..4].copy_from_slice(&0u32.to_be_bytes());
        }
        m
    }

    /// The file and every chunk with its offset in that file.
    pub fn bytes(&self, rng: &mut StdRng) -> (Vec<u8>, Vec<(u64, Vec<u8>)>) {
        let chunks: Vec<Vec<Vec<u8>>> = (0..self.tracks)
            .map(|_| {
                (0..self.chunks)
                    .map(|_| {
                        let n = rng.random_range(17..300);
                        random_bytes(rng, n)
                    })
                    .collect()
            })
            .collect();
        let ftyp = mp4_box(b"ftyp", b"isom\0\0\x02\0isomiso2");
        let split = if self.second_mdat_after_moov {
            self.chunks / 2
        } else {
            self.chunks
        };
        let mut first = Vec::new();
        let mut second = Vec::new();
        let mut rel = Vec::new();
        for (t, cs) in chunks.iter().enumerate() {
            for (i, c) in cs.iter().enumerate() {
                if i < split {
                    rel.push((t, true, first.len() as u64 + 8));
                    first.extend_from_slice(c);
                } else {
                    rel.push((t, false, second.len() as u64 + 8));
                    second.extend_from_slice(c);
                }
            }
        }
        let mdat1 = mp4_box(b"mdat", &first);
        let mdat2 = mp4_box(b"mdat", &second);
        let free = if self.free_before_moov {
            mp4_box(b"free", &[0u8; 33])
        } else {
            Vec::new()
        };
        let placeholder = self.moov(&vec![vec![0u64; self.chunks]; self.tracks]);
        let moov_len = placeholder.len() as u64;
        let ftyp_len = ftyp.len() as u64;
        let (m1, m2) = if self.moov_first {
            (
                ftyp_len + moov_len,
                ftyp_len + moov_len + mdat1.len() as u64,
            )
        } else {
            let m1 = ftyp_len;
            (m1, m1 + mdat1.len() as u64 + free.len() as u64 + moov_len)
        };
        let mut offsets = vec![Vec::new(); self.tracks];
        let mut all = Vec::new();
        for ((t, in_first, r), c) in rel.iter().zip(chunks.iter().flatten()) {
            let o = if *in_first { m1 + r } else { m2 + r };
            offsets[*t].push(o);
            all.push((o, c.clone()));
        }
        let moov = self.moov(&offsets);
        let mut out = ftyp;
        if self.moov_first {
            out.extend(moov);
            out.extend(mdat1);
            if self.second_mdat_after_moov {
                out.extend(mdat2);
            }
        } else {
            out.extend(mdat1);
            out.extend(free);
            out.extend(moov);
            if self.second_mdat_after_moov {
                out.extend(mdat2);
            }
        }
        (out, all)
    }
}

/// Chunk offsets of every `stco`/`co64` in the file's `moov`, in order.
pub fn mp4_offsets(file: &[u8]) -> Vec<u64> {
    fn walk(b: &[u8], out: &mut Vec<u64>) {
        let mut p = 0;
        while p + 8 <= b.len() {
            let mut size = u32::from_be_bytes(b[p..p + 4].try_into().unwrap()) as usize;
            if size == 0 {
                size = b.len() - p;
            }
            let kind = &b[p + 4..p + 8];
            let body = &b[p + 8..p + size];
            match kind {
                b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" => walk(body, out),
                b"stco" | b"co64" => {
                    let n = u32::from_be_bytes(body[4..8].try_into().unwrap()) as usize;
                    let w = if kind == b"co64" { 8 } else { 4 };
                    for i in 0..n {
                        let e = &body[8 + i * w..8 + (i + 1) * w];
                        out.push(if w == 8 {
                            u64::from_be_bytes(e.try_into().unwrap())
                        } else {
                            u32::from_be_bytes(e.try_into().unwrap()) as u64
                        });
                    }
                }
                _ => {}
            }
            p += size;
        }
    }
    let mut out = Vec::new();
    walk(file, &mut out);
    out
}

/// Top-level box types in order.
pub fn mp4_layout(file: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 8 <= file.len() {
        let mut size = u32::from_be_bytes(file[p..p + 4].try_into().unwrap()) as usize;
        if size == 0 {
            size = file.len() - p;
        }
        out.push(String::from_utf8_lossy(&file[p + 4..p + 8]).to_string());
        p += size;
    }
    out
}
