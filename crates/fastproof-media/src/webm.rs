//! WebM/Matroska: an index of the file in one bounded pass, the decision whether it needs a
//! seek index, and the rewrite with `SeekHead`, `Info` (with `Duration`), `Tracks`, the other
//! header elements, `Cues` and the clusters, whose bodies are copied unchanged.
//!
//! Only `Segment` and `Cluster` may have an unknown size; an unknown-size cluster ends at the
//! next element that belongs to the segment. A file that ends inside a cluster keeps the
//! blocks that are complete.

use std::fs::File;
use std::io::{Read, Seek, Write};
use std::ops::Range;
use std::path::Path;

use crate::ebml::{self, *};
use crate::{Budget, Error, Format, Limits, Outcome};

const MAX_EBML_HEADER: u64 = 4096;
const MAX_TRACKS: usize = 64;
const MAX_OTHERS: usize = 16;
/// Bytes charged to the memory budget per cluster and per cue point kept in the index, before
/// they are stored (an entry plus the slack of a growing vector).
const CLUSTER_COST: u64 = 128;
const CUE_COST: u64 = 80;

/// A block (`SimpleBlock`, or the `Block` of a `BlockGroup`) as found by [`scan`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub track: u64,
    /// Absolute timestamp in units of the segment's timestamp scale.
    pub time: i64,
    pub key: bool,
    /// Frames in the block (more than one with lacing).
    pub frames: u64,
    /// The whole element in the input.
    pub element: Range<u64>,
}

/// A cluster as found by [`scan`].
#[derive(Clone, Debug)]
pub struct Cluster {
    /// Offset of the element in the input.
    pub start: u64,
    /// The children that are complete.
    pub body: Range<u64>,
    pub timestamp: u64,
    /// Time of the first keyframe of the indexed track.
    pub first_key: Option<i64>,
    pub unknown_size: bool,
}

/// One position of a `CuePoint` (relative to the segment data).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CuePoint {
    pub time: u64,
    pub track: u64,
    pub position: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Track {
    pub number: u64,
    pub kind: u64,
    /// Nanoseconds per frame.
    pub default_duration: Option<u64>,
}

/// The index of a file.
#[derive(Debug, Default)]
pub struct Scan {
    pub doc_type: String,
    pub doc_type_version: u64,
    pub doc_type_read_version: u64,
    pub segment_data: u64,
    pub segment_unknown: bool,
    /// `Info` children other than `Duration`, `Void` and `CRC-32`, as they were.
    pub info_children: Vec<u8>,
    pub timestamp_scale: u64,
    pub duration: Option<f64>,
    /// The whole `Tracks` element.
    pub tracks: Option<Range<u64>>,
    pub track_list: Vec<Track>,
    /// The track the cues index: the first video track, else the first track.
    pub cue_track: Option<u64>,
    /// `Tags`, `Chapters`, `Attachments` elements, copied as they are.
    pub others: Vec<Range<u64>>,
    pub cues: Vec<CuePoint>,
    pub cues_after_clusters: bool,
    pub seek_head_lists_cues: bool,
    pub clusters: Vec<Cluster>,
    /// The input ends inside an element.
    pub truncated: bool,
    pub blocks: u64,
    /// The latest end of a block (its timestamp plus its duration when known).
    pub end_time: Option<i64>,
    /// Bytes charged to the memory budget while scanning (what this index holds).
    pub memory: u64,
}

impl Scan {
    /// Why the file does not seek well, `None` when it does.
    pub fn needs_index(&self) -> Option<&'static str> {
        if self.truncated {
            return Some("the file is truncated");
        }
        if self.segment_unknown {
            return Some("the segment has an unknown size");
        }
        if self.clusters.iter().any(|c| c.unknown_size) {
            return Some("a cluster has an unknown size");
        }
        if self.duration.is_none() {
            return Some("no duration");
        }
        if self.cues.is_empty() {
            return Some("no cues");
        }
        if self.cues_after_clusters && !self.seek_head_lists_cues {
            return Some("the cues follow the clusters and the seek head does not list them");
        }
        if !self.cues_point_at_clusters() {
            return Some("a cue does not point at a cluster");
        }
        None
    }

    /// Every cue position is the start of a cluster.
    pub fn cues_point_at_clusters(&self) -> bool {
        let starts: Vec<u64> = self
            .clusters
            .iter()
            .map(|c| c.start - self.segment_data)
            .collect();
        self.cues
            .iter()
            .all(|c| starts.binary_search(&c.position).is_ok())
    }

    fn default_duration(&self, track: u64) -> Option<u64> {
        self.track_list
            .iter()
            .find(|t| t.number == track)
            .and_then(|t| t.default_duration)
    }
}

/// Scans a whole file.
pub fn scan_file(
    path: &Path,
    limits: &Limits,
    visit: &mut dyn FnMut(&Block),
) -> Result<Scan, Error> {
    let mut r = Reader::new(File::open(path)?, limits)?;
    scan(&mut r, visit)
}

/// Builds the index of the input, calling `visit` for every complete block.
pub fn scan<R: Read + Seek>(
    r: &mut Reader<R>,
    visit: &mut dyn FnMut(&Block),
) -> Result<Scan, Error> {
    let mut budget = Budget::new(r.limits().max_memory_bytes);
    let mut s = Scan {
        timestamp_scale: 1_000_000,
        ..Scan::default()
    };
    r.seek_to(0)?;
    let h = r.header()?;
    if h.id != EBML {
        return Err(Error::invalid("not an EBML file"));
    }
    let size = h
        .size
        .filter(|n| *n <= MAX_EBML_HEADER)
        .ok_or_else(|| Error::invalid("EBML header of unknown or excessive size"))?;
    let buf = r.read_vec(size)?;
    ebml_header(&buf, &mut s)?;

    let seg = r.header()?;
    if seg.id != SEGMENT {
        return Err(Error::invalid("no segment after the EBML header"));
    }
    s.segment_data = seg.data;
    s.segment_unknown = seg.size.is_none();
    let seg_end = match seg.end() {
        Some(e) if e <= r.len() => e,
        Some(_) => {
            s.truncated = true;
            r.len()
        }
        None => r.len(),
    };
    let max_header = r.limits().max_header_bytes;
    let mut pos = seg.data;
    let mut seen_info = false;
    while pos < seg_end {
        r.seek_to(pos)?;
        let h = match r.header() {
            Ok(h) if h.data <= seg_end => h,
            Ok(_) | Err(Error::Truncated) => {
                s.truncated = true;
                break;
            }
            Err(e) => return Err(e),
        };
        if h.id == CLUSTER {
            let (next, ended) = cluster(r, &h, seg_end, &mut s, &mut budget, visit)?;
            if ended {
                s.truncated = true;
                break;
            }
            pos = next;
            continue;
        }
        let Some(size) = h.size else {
            return Err(Error::invalid(
                "an element other than a Segment or Cluster has an unknown size",
            ));
        };
        let end = h
            .data
            .checked_add(size)
            .ok_or_else(|| Error::invalid("element size overflows"))?;
        if end > seg_end {
            s.truncated = true;
            break;
        }
        match h.id {
            INFO | TRACKS | SEEK_HEAD | CUES => {
                let cap = if h.id == CUES {
                    r.limits().max_memory_bytes
                } else {
                    max_header
                };
                if size > cap {
                    return Err(Error::limit("header element too large"));
                }
                budget.charge(size)?;
                let buf = r.read_vec(size)?;
                match h.id {
                    INFO if !seen_info => {
                        seen_info = true;
                        info(&buf, &mut s, &mut budget)?;
                    }
                    INFO => {}
                    TRACKS => {
                        if s.tracks.is_some() {
                            return Err(Error::invalid("more than one Tracks element"));
                        }
                        if !s.clusters.is_empty() {
                            return Err(Error::invalid("Tracks after the first Cluster"));
                        }
                        tracks(&buf, &mut s)?;
                        s.tracks = Some(h.start..end);
                    }
                    SEEK_HEAD => seek_head(&buf, &mut s)?,
                    _ => {
                        cues(&buf, &mut s, &mut budget)?;
                        if !s.clusters.is_empty() {
                            s.cues_after_clusters = true;
                        }
                    }
                }
            }
            TAGS | CHAPTERS | ATTACHMENTS => {
                if s.others.len() >= MAX_OTHERS {
                    return Err(Error::limit("too many header elements"));
                }
                s.others.push(h.start..end);
            }
            EBML | SEGMENT => return Err(Error::invalid("a nested EBML header or Segment")),
            _ => {}
        }
        pos = end;
    }
    s.memory = budget.used();
    Ok(s)
}

fn ebml_header(buf: &[u8], s: &mut Scan) -> Result<(), Error> {
    let mut doc_type = None;
    s.doc_type_version = 1;
    s.doc_type_read_version = 1;
    for c in ebml::children(buf) {
        let (id, _, v) = c?;
        match id {
            EBML_READ_VERSION if read_uint(v)? > 1 => {
                return Err(Error::invalid("unsupported EBMLReadVersion"));
            }
            EBML_MAX_ID_LENGTH if read_uint(v)? > 4 => {
                return Err(Error::invalid("unsupported EBMLMaxIDLength"));
            }
            EBML_MAX_SIZE_LENGTH if read_uint(v)? > 8 => {
                return Err(Error::invalid("unsupported EBMLMaxSizeLength"));
            }
            DOC_TYPE => {
                let t = String::from_utf8_lossy(v)
                    .trim_end_matches('\0')
                    .to_string();
                doc_type = Some(t);
            }
            DOC_TYPE_VERSION => s.doc_type_version = read_uint(v)?.clamp(1, 4),
            DOC_TYPE_READ_VERSION => s.doc_type_read_version = read_uint(v)?,
            _ => {}
        }
    }
    match doc_type.as_deref() {
        Some("webm") | Some("matroska") => {}
        _ => return Err(Error::invalid("DocType is neither webm nor matroska")),
    }
    if s.doc_type_read_version > 4 || s.doc_type_read_version == 0 {
        return Err(Error::invalid("unsupported DocTypeReadVersion"));
    }
    s.doc_type = doc_type.unwrap_or_default();
    Ok(())
}

fn info(buf: &[u8], s: &mut Scan, budget: &mut Budget) -> Result<(), Error> {
    for c in ebml::children(buf) {
        let (id, whole, v) = c?;
        match id {
            TIMESTAMP_SCALE => {
                s.timestamp_scale = read_uint(v)?;
                if s.timestamp_scale == 0 {
                    return Err(Error::invalid("TimestampScale 0"));
                }
                budget.charge(whole.len() as u64)?;
                s.info_children.extend_from_slice(whole);
            }
            DURATION => {
                let d = read_float(v)?;
                s.duration = (d.is_finite() && d > 0.0).then_some(d);
            }
            VOID | CRC32 => {}
            _ => {
                budget.charge(whole.len() as u64)?;
                s.info_children.extend_from_slice(whole);
            }
        }
    }
    Ok(())
}

fn tracks(buf: &[u8], s: &mut Scan) -> Result<(), Error> {
    for c in ebml::children(buf) {
        let (id, _, v) = c?;
        if id != TRACK_ENTRY {
            continue;
        }
        if s.track_list.len() >= MAX_TRACKS {
            return Err(Error::limit("too many tracks"));
        }
        let mut t = Track::default();
        for c in ebml::children(v) {
            let (id, _, v) = c?;
            match id {
                TRACK_NUMBER => t.number = read_uint(v)?,
                TRACK_TYPE => t.kind = read_uint(v)?,
                DEFAULT_DURATION => t.default_duration = Some(read_uint(v)?).filter(|d| *d > 0),
                _ => {}
            }
        }
        if t.number == 0 {
            return Err(Error::invalid("track without a number"));
        }
        s.track_list.push(t);
    }
    s.cue_track = s
        .track_list
        .iter()
        .find(|t| t.kind == 1)
        .or(s.track_list.first())
        .map(|t| t.number);
    if s.cue_track.is_none() {
        return Err(Error::invalid("no tracks"));
    }
    Ok(())
}

fn seek_head(buf: &[u8], s: &mut Scan) -> Result<(), Error> {
    let cues_id = CUES.to_be_bytes();
    for c in ebml::children(buf) {
        let (id, _, v) = c?;
        if id != SEEK {
            continue;
        }
        for c in ebml::children(v) {
            let (id, _, v) = c?;
            if id == SEEK_ID && v == cues_id {
                s.seek_head_lists_cues = true;
            }
        }
    }
    Ok(())
}

fn cues(buf: &[u8], s: &mut Scan, budget: &mut Budget) -> Result<(), Error> {
    for c in ebml::children(buf) {
        let (id, _, v) = c?;
        if id != CUE_POINT {
            continue;
        }
        let mut time = None;
        for c in ebml::children(v) {
            let (id, _, v) = c?;
            if id == CUE_TIME {
                time = Some(read_uint(v)?);
            }
        }
        let time = time.ok_or_else(|| Error::invalid("CuePoint without CueTime"))?;
        for c in ebml::children(v) {
            let (id, _, v) = c?;
            if id != CUE_TRACK_POSITIONS {
                continue;
            }
            let (mut track, mut pos) = (None, None);
            for c in ebml::children(v) {
                let (id, _, v) = c?;
                match id {
                    CUE_TRACK => track = Some(read_uint(v)?),
                    CUE_CLUSTER_POSITION => pos = Some(read_uint(v)?),
                    _ => {}
                }
            }
            if let (Some(track), Some(position)) = (track, pos) {
                budget.charge(CUE_COST)?;
                s.cues.push(CuePoint {
                    time,
                    track,
                    position,
                });
            }
        }
    }
    Ok(())
}

/// Absolute time of a block: the cluster timestamp plus the block's relative one.
fn block_time(cluster_ts: u64, rel: i16) -> Result<i64, Error> {
    i64::try_from(cluster_ts)
        .ok()
        .and_then(|t| t.checked_add(i64::from(rel)))
        .ok_or_else(|| Error::invalid("block time overflows"))
}

/// Parses a cluster; returns where the next element of the segment starts, and whether the
/// input ended inside the cluster.
fn cluster<R: Read + Seek>(
    r: &mut Reader<R>,
    h: &Header,
    seg_end: u64,
    s: &mut Scan,
    budget: &mut Budget,
    visit: &mut dyn FnMut(&Block),
) -> Result<(u64, bool), Error> {
    if s.tracks.is_none() {
        return Err(Error::invalid("Cluster before Tracks"));
    }
    budget.charge(CLUSTER_COST)?;
    let limit_end = match h.end() {
        Some(e) if e <= seg_end => e,
        Some(_) => seg_end,
        None => seg_end,
    };
    let mut pos = h.data;
    let mut ts: Option<u64> = None;
    let mut first_key = None;
    let mut ended = false;
    while pos < limit_end {
        r.seek_to(pos)?;
        let c = match r.header() {
            Ok(c) => c,
            Err(Error::Truncated) => {
                ended = true;
                break;
            }
            Err(e) => return Err(e),
        };
        if h.size.is_none() && ebml::is_top_level(c.id) {
            break;
        }
        let Some(size) = c.size else {
            return Err(Error::invalid(
                "an element of unknown size inside a Cluster",
            ));
        };
        let end = c
            .data
            .checked_add(size)
            .ok_or_else(|| Error::invalid("element size overflows"))?;
        if end > limit_end {
            if end > r.len() || limit_end < h.end().unwrap_or(limit_end) {
                ended = true;
                break;
            }
            return Err(Error::invalid("an element exceeds its Cluster"));
        }
        match c.id {
            TIMESTAMP => {
                if size > 8 {
                    return Err(Error::invalid("cluster timestamp longer than 8 bytes"));
                }
                ts = Some(read_uint(&r.read_vec(size)?)?);
            }
            SIMPLE_BLOCK | BLOCK_GROUP => {
                let cluster_ts =
                    ts.ok_or_else(|| Error::invalid("a block before the cluster timestamp"))?;
                let (b, duration) = if c.id == SIMPLE_BLOCK {
                    let (track, rel, flags, frames) = block_header(r, c.data, size)?;
                    let time = block_time(cluster_ts, rel)?;
                    let key = flags & 0x80 != 0;
                    let element = c.start..end;
                    (
                        Block {
                            track,
                            time,
                            key,
                            frames,
                            element,
                        },
                        None,
                    )
                } else {
                    block_group(r, &c, end, cluster_ts)?
                };
                s.blocks += 1;
                let span = match duration {
                    Some(d) => i128::from(d),
                    None => s
                        .default_duration(b.track)
                        .map(|ns| {
                            (i128::from(ns) * i128::from(b.frames)
                                + i128::from(s.timestamp_scale / 2))
                                / i128::from(s.timestamp_scale)
                        })
                        .unwrap_or(0),
                };
                let block_end = (i128::from(b.time) + span).clamp(0, i64::MAX as i128) as i64;
                s.end_time = Some(s.end_time.map_or(block_end, |e| e.max(block_end)));
                if b.key && first_key.is_none() && Some(b.track) == s.cue_track {
                    first_key = Some(b.time);
                }
                visit(&b);
            }
            _ => {}
        }
        pos = end;
    }
    if h.end().is_some_and(|e| e > limit_end) {
        ended = true;
    }
    let Some(timestamp) = ts else {
        if ended {
            return Ok((pos, true));
        }
        return Err(Error::invalid("a Cluster without a timestamp"));
    };
    s.clusters.push(Cluster {
        start: h.start,
        body: h.data..pos,
        timestamp,
        first_key,
        unknown_size: h.size.is_none(),
    });
    Ok((pos, ended))
}

/// Track number, relative timestamp, flags and frame count of a block.
fn block_header<R: Read + Seek>(
    r: &mut Reader<R>,
    data: u64,
    size: u64,
) -> Result<(u64, i16, u8, u64), Error> {
    if size < 4 {
        return Err(Error::invalid("block shorter than its header"));
    }
    let n = size.min(16) as usize;
    let mut b = [0u8; 16];
    r.seek_to(data)?;
    r.read_exact(&mut b[..n])?;
    let (track, tl) = match decode_size(&b[..n]) {
        Ok((Some(t), tl)) if t > 0 => (t, tl),
        Ok(_) => return Err(Error::invalid("block with an invalid track number")),
        Err(Error::Truncated) => return Err(Error::invalid("block shorter than its header")),
        Err(e) => return Err(e),
    };
    if tl + 3 > n {
        return Err(Error::invalid("block shorter than its header"));
    }
    let rel = i16::from_be_bytes([b[tl], b[tl + 1]]);
    let flags = b[tl + 2];
    let frames = if (flags >> 1) & 3 != 0 {
        if tl + 4 > n {
            return Err(Error::invalid("laced block without a frame count"));
        }
        u64::from(b[tl + 3]) + 1
    } else {
        1
    };
    Ok((track, rel, flags, frames))
}

fn block_group<R: Read + Seek>(
    r: &mut Reader<R>,
    g: &Header,
    end: u64,
    cluster_ts: u64,
) -> Result<(Block, Option<u64>), Error> {
    let mut pos = g.data;
    let mut block = None;
    let mut duration = None;
    let mut referenced = false;
    while pos < end {
        r.seek_to(pos)?;
        let c = r.header()?;
        let child_end = c
            .end()
            .filter(|e| *e <= end)
            .ok_or_else(|| Error::invalid("a BlockGroup child of unknown or excessive size"))?;
        let size = child_end - c.data;
        match c.id {
            BLOCK => block = Some(block_header(r, c.data, size)?),
            BLOCK_DURATION => {
                if size > 8 {
                    return Err(Error::invalid("BlockDuration longer than 8 bytes"));
                }
                duration = Some(read_uint(&r.read_vec(size)?)?);
            }
            REFERENCE_BLOCK => referenced = true,
            _ => {}
        }
        pos = child_end;
    }
    let (track, rel, _, frames) =
        block.ok_or_else(|| Error::invalid("a BlockGroup without a Block"))?;
    Ok((
        Block {
            track,
            time: block_time(cluster_ts, rel)?,
            key: !referenced,
            frames,
            element: g.start..end,
        },
        duration,
    ))
}

fn cue_point_len(time: u64, track: u64) -> u64 {
    let positions =
        element_len(CUE_TRACK, uint_len(track) as u64) + element_len(CUE_CLUSTER_POSITION, 8);
    element_len(
        CUE_POINT,
        element_len(CUE_TIME, uint_len(time) as u64) + element_len(CUE_TRACK_POSITIONS, positions),
    )
}

fn seek_entry(out: &mut Vec<u8>, id: u32, position: u64) {
    let mut e = Vec::with_capacity(24);
    let mut idb = Vec::with_capacity(4);
    write_id(&mut idb, id);
    write_element(&mut e, SEEK_ID, &idb);
    write_uint8(&mut e, SEEK_POSITION, position);
    write_element(out, SEEK, &e);
}

fn seek_entry_len(id: u32) -> u64 {
    element_len(
        SEEK,
        element_len(SEEK_ID, id_len(id) as u64) + element_len(SEEK_POSITION, 8),
    )
}

/// The cue points of the output: the first keyframe of the indexed track in every cluster
/// that has one, in time order, as `(time, cluster index)`.
fn cue_points(s: &Scan) -> Vec<(u64, usize)> {
    let mut v: Vec<(u64, usize)> = s
        .clusters
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.first_key.map(|t| (t.max(0) as u64, i)))
        .collect();
    v.sort_by_key(|p| p.0);
    v
}

/// Writes the seekable file. `r` is the input the scan was made of.
pub fn write<R: Read + Seek, W: Write>(
    r: &mut Reader<R>,
    s: &Scan,
    out: &mut W,
) -> Result<u64, Error> {
    let tracks = s
        .tracks
        .clone()
        .ok_or_else(|| Error::invalid("no Tracks"))?;
    let cue_track = s.cue_track.ok_or_else(|| Error::invalid("no tracks"))?;
    let end_time = s.end_time.ok_or_else(|| Error::invalid("no blocks"))?;
    let mut budget = Budget::new(r.limits().max_memory_bytes);
    budget.charge(s.memory)?;
    budget.charge(s.clusters.len() as u64 * 16)?;
    let points = cue_points(s);
    if points.is_empty() {
        return Err(Error::invalid("no keyframe on the indexed track"));
    }

    let info_payload = s.info_children.len() as u64 + element_len(DURATION, 8);
    let info_len = element_len(INFO, info_payload);
    let tracks_len = tracks.end - tracks.start;
    let others_len: u64 = s.others.iter().map(|o| o.end - o.start).sum();
    let cues_payload: u64 = points
        .iter()
        .map(|(t, _)| cue_point_len(*t, cue_track))
        .sum();
    let cues_len = element_len(CUES, cues_payload);
    let mut entries = vec![INFO, TRACKS];
    entries.extend(s.others.iter().map(|_| TAGS));
    entries.push(CUES);
    let seek_payload: u64 = entries.iter().map(|id| seek_entry_len(*id)).sum();
    let seek_len = element_len(SEEK_HEAD, seek_payload);

    let info_pos = seek_len;
    let tracks_pos = info_pos + info_len;
    let others_pos = tracks_pos + tracks_len;
    let cues_pos = others_pos + others_len;
    let clusters_pos = cues_pos + cues_len;
    let cluster_header = (id_len(CLUSTER) + 8) as u64;
    let mut cluster_pos = Vec::with_capacity(s.clusters.len());
    let mut at = clusters_pos;
    for c in &s.clusters {
        budget.charge(8)?;
        cluster_pos.push(at);
        at += cluster_header + (c.body.end - c.body.start);
    }
    let segment_len = at;

    let mut head = Vec::with_capacity(256);
    let mut e = Vec::with_capacity(64);
    write_uint(&mut e, EBML_VERSION, 1);
    write_uint(&mut e, EBML_READ_VERSION, 1);
    write_uint(&mut e, EBML_MAX_ID_LENGTH, 4);
    write_uint(&mut e, EBML_MAX_SIZE_LENGTH, 8);
    write_element(&mut e, DOC_TYPE, s.doc_type.as_bytes());
    write_uint(&mut e, DOC_TYPE_VERSION, s.doc_type_version);
    write_uint(&mut e, DOC_TYPE_READ_VERSION, s.doc_type_read_version);
    write_element(&mut head, EBML, &e);
    write_id(&mut head, SEGMENT);
    write_size8(&mut head, segment_len);
    let total = head.len() as u64 + segment_len;

    let mut seek = Vec::with_capacity(seek_payload as usize);
    seek_entry(&mut seek, INFO, info_pos);
    seek_entry(&mut seek, TRACKS, tracks_pos);
    let mut p = others_pos;
    for o in &s.others {
        r.seek_to(o.start)?;
        let id = r.header()?.id;
        seek_entry(&mut seek, id, p);
        p += o.end - o.start;
    }
    seek_entry(&mut seek, CUES, cues_pos);
    debug_assert_eq!(seek.len() as u64, seek_payload);
    write_element(&mut head, SEEK_HEAD, &seek);

    budget.charge(info_len)?;
    let mut info = Vec::with_capacity(info_payload as usize);
    info.extend_from_slice(&s.info_children);
    write_float(&mut info, DURATION, end_time as f64);
    write_element(&mut head, INFO, &info);
    out.write_all(&head)?;

    r.copy_to(tracks.start, tracks_len, out)?;
    for o in &s.others {
        r.copy_to(o.start, o.end - o.start, out)?;
    }

    budget.charge(cues_payload)?;
    let mut cues = Vec::with_capacity(cues_payload as usize);
    for (time, idx) in &points {
        let mut tp = Vec::with_capacity(16);
        write_uint(&mut tp, CUE_TRACK, cue_track);
        write_uint8(&mut tp, CUE_CLUSTER_POSITION, cluster_pos[*idx]);
        let mut cp = Vec::with_capacity(24);
        write_uint(&mut cp, CUE_TIME, *time);
        write_element(&mut cp, CUE_TRACK_POSITIONS, &tp);
        write_element(&mut cues, CUE_POINT, &cp);
    }
    debug_assert_eq!(cues.len() as u64, cues_payload);
    let mut ch = Vec::with_capacity(16);
    write_id(&mut ch, CUES);
    write_size(&mut ch, cues_payload);
    out.write_all(&ch)?;
    out.write_all(&cues)?;
    drop(cues);

    for c in &s.clusters {
        let mut hdr = Vec::with_capacity(12);
        write_id(&mut hdr, CLUSTER);
        write_size8(&mut hdr, c.body.end - c.body.start);
        out.write_all(&hdr)?;
        r.copy_to(c.body.start, c.body.end - c.body.start, out)?;
    }
    Ok(total)
}

/// Writes a seekable copy of the WebM file `input` to `output` when it needs one, then checks
/// the copy: it must need nothing, hold the same clusters and blocks, end at the same time and
/// have a cue for every cluster with a keyframe.
pub fn prepare_file(input: &Path, output: &Path, limits: &Limits) -> Result<Outcome, Error> {
    let mut r = Reader::new(File::open(input)?, limits)?;
    let s = scan(&mut r, &mut |_| {})?;
    if s.blocks == 0 || s.clusters.is_empty() {
        return Ok(Outcome::Unsupported("no frames".into()));
    }
    if s.needs_index().is_none() {
        return Ok(Outcome::AlreadySeekable(Format::Webm));
    }
    if cue_points(&s).is_empty() {
        return Ok(Outcome::Unsupported(
            "no keyframe on the indexed track".into(),
        ));
    }
    let expected = crate::write_file(output, |w| write(&mut r, &s, w).map(|_| ()))?;
    drop(r);
    let (clusters, blocks, end_time, cues) =
        (s.clusters.len(), s.blocks, s.end_time, cue_points(&s).len());
    drop(s);
    let check_limits = Limits {
        max_input_bytes: u64::MAX,
        ..limits.clone()
    };
    let o = scan_file(output, &check_limits, &mut |_| {})?;
    let fail = |m: &str| {
        Err(Error::invalid(format!(
            "the prepared copy failed its check: {m}"
        )))
    };
    if let Some(why) = o.needs_index() {
        return fail(why);
    }
    if o.clusters.len() != clusters || o.blocks != blocks {
        return fail("clusters or blocks differ");
    }
    if o.end_time != end_time || o.duration != end_time.map(|t| t as f64) {
        return fail("duration differs");
    }
    if o.cues.len() != cues {
        return fail("cue count differs");
    }
    Ok(Outcome::Prepared {
        format: Format::Webm,
        bytes: expected,
    })
}

/// Parses a prepared file again (the caller already checked it in [`prepare_file`]).
pub fn verify(path: &Path, limits: &Limits) -> Result<Scan, Error> {
    let s = scan_file(path, limits, &mut |_| {})?;
    if let Some(why) = s.needs_index() {
        return Err(Error::invalid(why));
    }
    Ok(s)
}
