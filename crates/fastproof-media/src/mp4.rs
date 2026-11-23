//! MP4/MOV "faststart": when `moov` follows the media data, a browser must fetch the end of
//! the file before it can play or seek. The copy places `moov` in front of the first `mdat`
//! and shifts every chunk offset (`stco`, `co64`) that points into the moved range. Files whose
//! `moov` holds other absolute offsets (`iloc`, `saio`) or fragments (`mvex`) are left alone.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::{Budget, Error, Format, Limits, Outcome};

const MAX_BOXES: u64 = 100_000;
const MAX_DEPTH: u32 = 8;
const CONTAINERS: [&[u8; 4]; 6] = [b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts"];
const SAMPLED_OFFSETS: usize = 2000;
const NEEDS_CO64: &str = "chunk offsets would need 64 bits";

/// Box types a file may start with.
pub fn is_top_box_type(t: &[u8]) -> bool {
    matches!(t, b"ftyp" | b"moov" | b"mdat" | b"free" | b"skip" | b"wide")
}

/// A top-level box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopBox {
    pub kind: [u8; 4],
    pub start: u64,
    pub header: u64,
    pub end: u64,
}

/// Lists the top-level boxes of the input.
pub fn top_boxes<R: Read + Seek>(
    r: &mut R,
    len: u64,
    limits: &Limits,
) -> Result<Vec<TopBox>, Error> {
    let mut out = Vec::new();
    let mut pos = 0u64;
    while pos < len {
        if out.len() as u64 >= MAX_BOXES {
            return Err(Error::limit("too many boxes"));
        }
        if out.len() % 256 == 0 {
            limits.check_deadline()?;
        }
        if len - pos < 8 {
            return Err(Error::Truncated);
        }
        r.seek(SeekFrom::Start(pos))?;
        let mut h = [0u8; 16];
        r.read_exact(&mut h[..8])?;
        let size32 = u32::from_be_bytes([h[0], h[1], h[2], h[3]]);
        let kind = [h[4], h[5], h[6], h[7]];
        if !kind.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
            return Err(Error::invalid("box type is not printable"));
        }
        let (size, header) = match size32 {
            0 => (len - pos, 8),
            1 => {
                if len - pos < 16 {
                    return Err(Error::Truncated);
                }
                r.read_exact(&mut h[8..16])?;
                let mut b = [0u8; 8];
                b.copy_from_slice(&h[8..16]);
                (u64::from_be_bytes(b), 16)
            }
            n => (u64::from(n), 8),
        };
        if size < header {
            return Err(Error::invalid("box smaller than its header"));
        }
        let end = pos
            .checked_add(size)
            .ok_or_else(|| Error::invalid("box size overflows"))?;
        if end > len {
            return Err(Error::Truncated);
        }
        out.push(TopBox {
            kind,
            start: pos,
            header,
            end,
        });
        pos = end;
    }
    Ok(out)
}

/// A child box: type, start, header length and end, relative to its parent's payload.
type Child = ([u8; 4], usize, usize, usize);

/// Visitor of chunk-offset tables.
type TableFn<'a> = dyn FnMut(&[u8; 4], &mut [u8]) -> Result<(), Error> + 'a;

/// Children of a box payload in memory: `(type, start, header length, end)` relative to `buf`.
fn children(buf: &[u8]) -> Result<Vec<Child>, Error> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < buf.len() {
        if buf.len() - pos < 8 {
            return Err(Error::invalid("box header exceeds its parent"));
        }
        let size32 = u32::from_be_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]);
        let kind = [buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]];
        let (size, header) = match size32 {
            0 => (buf.len() - pos, 8),
            1 => {
                if buf.len() - pos < 16 {
                    return Err(Error::invalid("box header exceeds its parent"));
                }
                let mut b = [0u8; 8];
                b.copy_from_slice(&buf[pos + 8..pos + 16]);
                let s = usize::try_from(u64::from_be_bytes(b))
                    .map_err(|_| Error::invalid("box size overflows"))?;
                (s, 16)
            }
            n => (n as usize, 8),
        };
        if size < header || size > buf.len() - pos {
            return Err(Error::invalid("box exceeds its parent"));
        }
        out.push((kind, pos, header, pos + size));
        pos += size;
    }
    Ok(out)
}

/// Calls `f` for every chunk-offset table (`stco` with 4-byte, `co64` with 8-byte entries)
/// under `moov`; fails on boxes whose offsets cannot be moved.
fn offset_tables(
    buf: &mut [u8],
    depth: u32,
    boxes: &mut u64,
    f: &mut TableFn<'_>,
) -> Result<(), Error> {
    if depth > MAX_DEPTH {
        return Err(Error::limit("boxes nested too deeply"));
    }
    for (kind, start, header, end) in children(buf)? {
        *boxes += 1;
        if *boxes > MAX_BOXES {
            return Err(Error::limit("too many boxes"));
        }
        let payload = &mut buf[start + header..end];
        match &kind {
            b"mvex" => return Err(Error::Invalid("fragmented".into())),
            b"iloc" | b"saio" => {
                return Err(Error::Invalid("absolute offsets outside stco".into()));
            }
            b"stco" | b"co64" => f(&kind, payload)?,
            b"meta" if payload.len() >= 4 => offset_tables(&mut payload[4..], depth + 1, boxes, f)?,
            k if CONTAINERS.contains(&k) => offset_tables(payload, depth + 1, boxes, f)?,
            _ => {}
        }
    }
    Ok(())
}

/// Entries of a chunk-offset table: `(entry width, count)`, checked against the payload.
fn table(kind: &[u8; 4], payload: &[u8]) -> Result<(usize, usize), Error> {
    if payload.len() < 8 {
        return Err(Error::invalid("chunk offset table too short"));
    }
    let count = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]) as usize;
    let width = if kind == b"co64" { 8 } else { 4 };
    if count
        .checked_mul(width)
        .and_then(|n| n.checked_add(8))
        .is_none_or(|n| n > payload.len())
    {
        return Err(Error::invalid("chunk offset table longer than its box"));
    }
    Ok((width, count))
}

fn entry(payload: &[u8], width: usize, i: usize) -> u64 {
    let at = 8 + i * width;
    if width == 8 {
        let mut b = [0u8; 8];
        b.copy_from_slice(&payload[at..at + 8]);
        u64::from_be_bytes(b)
    } else {
        u64::from(u32::from_be_bytes([
            payload[at],
            payload[at + 1],
            payload[at + 2],
            payload[at + 3],
        ]))
    }
}

/// Every chunk offset of a `moov` box (whole box in `buf`), in order.
fn offsets(buf: &mut [u8], header: usize) -> Result<Vec<u64>, Error> {
    let mut out = Vec::new();
    let mut boxes = 0;
    offset_tables(&mut buf[header..], 1, &mut boxes, &mut |kind, p| {
        let (w, n) = table(kind, p)?;
        out.extend((0..n).map(|i| entry(p, w, i)));
        Ok(())
    })?;
    Ok(out)
}

fn copy_range<R: Read + Seek, W: Write>(
    r: &mut R,
    from: u64,
    n: u64,
    out: &mut W,
    limits: &Limits,
) -> Result<(), Error> {
    r.seek(SeekFrom::Start(from))?;
    let mut left = n;
    let mut buf = vec![0u8; 64 * 1024];
    while left > 0 {
        limits.check_deadline()?;
        let k = left.min(buf.len() as u64) as usize;
        r.read_exact(&mut buf[..k])?;
        out.write_all(&buf[..k])?;
        left -= k as u64;
    }
    Ok(())
}

/// Writes a faststart copy of the MP4 file `input` to `output` when `moov` follows the media
/// data, then checks the copy: `moov` comes first and every sampled chunk offset of the copy
/// points at the same bytes as in the input.
pub fn prepare_file(input: &Path, output: &Path, limits: &Limits) -> Result<Outcome, Error> {
    let mut r = BufReader::with_capacity(64 * 1024, File::open(input)?);
    let len = r.seek(SeekFrom::End(0))?;
    if len > limits.max_input_bytes {
        return Err(Error::limit("input larger than the allowed size"));
    }
    let boxes = top_boxes(&mut r, len, limits)?;
    if boxes.iter().any(|b| &b.kind == b"moof") {
        return Ok(Outcome::AlreadySeekable(Format::Mp4));
    }
    let moovs: Vec<&TopBox> = boxes.iter().filter(|b| &b.kind == b"moov").collect();
    let moov = match moovs.as_slice() {
        [m] => **m,
        [] => return Ok(Outcome::Unsupported("no moov box".into())),
        _ => return Err(Error::invalid("more than one moov box")),
    };
    let Some(mdat) = boxes.iter().find(|b| &b.kind == b"mdat").copied() else {
        return Ok(Outcome::Unsupported("no media data".into()));
    };
    if moov.start < mdat.start {
        return Ok(Outcome::AlreadySeekable(Format::Mp4));
    }
    let moov_len = moov.end - moov.start;
    if moov_len > limits.max_memory_bytes {
        return Err(Error::limit("moov box larger than the memory budget"));
    }
    let mut budget = Budget::new(limits.max_memory_bytes);
    budget.charge(moov_len)?;
    let mut buf = vec![0u8; moov_len as usize];
    r.seek(SeekFrom::Start(moov.start))?;
    r.read_exact(&mut buf)?;
    if u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) == 0 {
        let n = u32::try_from(moov_len).map_err(|_| Error::invalid("moov too large"))?;
        buf[..4].copy_from_slice(&n.to_be_bytes());
    }
    let shifted = mdat.start..moov.start;
    let header = moov.header as usize;
    let original = match offsets(&mut buf, header) {
        Ok(v) => v,
        Err(Error::Invalid(m)) if m == "fragmented" => {
            return Ok(Outcome::AlreadySeekable(Format::Mp4));
        }
        Err(Error::Invalid(m)) if m.starts_with("absolute offsets") => {
            return Ok(Outcome::Unsupported(m));
        }
        Err(e) => return Err(e),
    };
    budget.charge(original.len() as u64 * 8)?;
    if original.iter().any(|o| *o >= len) {
        return Err(Error::invalid("a chunk offset lies outside the file"));
    }
    let mut boxes_seen = 0;
    let patched = offset_tables(&mut buf[header..], 1, &mut boxes_seen, &mut |kind, p| {
        let (w, n) = table(kind, p)?;
        for i in 0..n {
            let o = entry(p, w, i);
            if !shifted.contains(&o) {
                continue;
            }
            let v = o + moov_len;
            let at = 8 + i * w;
            if w == 8 {
                p[at..at + 8].copy_from_slice(&v.to_be_bytes());
            } else {
                let v = u32::try_from(v).map_err(|_| Error::invalid(NEEDS_CO64))?;
                p[at..at + 4].copy_from_slice(&v.to_be_bytes());
            }
        }
        Ok(())
    });
    match patched {
        Err(Error::Invalid(m)) if m == NEEDS_CO64 => return Ok(Outcome::Unsupported(m)),
        r => r?,
    }

    let bytes = crate::write_file(output, |w| {
        copy_range(&mut r, 0, mdat.start, w, limits)?;
        w.write_all(&buf)?;
        copy_range(&mut r, mdat.start, moov.start - mdat.start, w, limits)?;
        copy_range(&mut r, moov.end, len - moov.end, w, limits)
    })?;
    let moved = offsets(&mut buf, header)?;
    drop(buf);
    check(&mut r, output, len, &original, &moved, limits)?;
    Ok(Outcome::Prepared {
        format: Format::Mp4,
        bytes,
    })
}

fn check<R: Read + Seek>(
    input: &mut R,
    output: &Path,
    in_len: u64,
    original: &[u64],
    moved: &[u64],
    limits: &Limits,
) -> Result<(), Error> {
    let fail = |m: &str| Error::invalid(format!("the prepared copy failed its check: {m}"));
    let mut out = BufReader::new(File::open(output)?);
    let out_len = out.seek(SeekFrom::End(0))?;
    if out_len != in_len {
        return Err(fail("size differs"));
    }
    let boxes = top_boxes(&mut out, out_len, limits)?;
    let moov = boxes.iter().position(|b| &b.kind == b"moov");
    let mdat = boxes.iter().position(|b| &b.kind == b"mdat");
    if moov.is_none() || mdat.is_none() || moov > mdat {
        return Err(fail("moov does not precede the media data"));
    }
    if original.len() != moved.len() {
        return Err(fail("chunk count differs"));
    }
    let step = (original.len() / SAMPLED_OFFSETS).max(1);
    let mut a = [0u8; 16];
    let mut b = [0u8; 16];
    for i in (0..original.len()).step_by(step) {
        limits.check_deadline()?;
        let n = (in_len - original[i]).min(16) as usize;
        if moved[i] + n as u64 > out_len {
            return Err(fail("a chunk offset lies outside the copy"));
        }
        input.seek(SeekFrom::Start(original[i]))?;
        input.read_exact(&mut a[..n])?;
        out.seek(SeekFrom::Start(moved[i]))?;
        out.read_exact(&mut b[..n])?;
        if a[..n] != b[..n] {
            return Err(fail("a chunk offset points at other bytes"));
        }
    }
    Ok(())
}
