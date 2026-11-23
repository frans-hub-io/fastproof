//! The EBML primitives Matroska is built on: variable-size integers for element IDs and sizes,
//! a bounded reader of element headers, and writers of elements.

use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};

use crate::{Error, Limits};

pub const EBML: u32 = 0x1A45_DFA3;
pub const EBML_VERSION: u32 = 0x4286;
pub const EBML_READ_VERSION: u32 = 0x42F7;
pub const EBML_MAX_ID_LENGTH: u32 = 0x42F2;
pub const EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
pub const DOC_TYPE: u32 = 0x4282;
pub const DOC_TYPE_VERSION: u32 = 0x4287;
pub const DOC_TYPE_READ_VERSION: u32 = 0x4285;

pub const SEGMENT: u32 = 0x1853_8067;
pub const SEEK_HEAD: u32 = 0x114D_9B74;
pub const SEEK: u32 = 0x4DBB;
pub const SEEK_ID: u32 = 0x53AB;
pub const SEEK_POSITION: u32 = 0x53AC;
pub const INFO: u32 = 0x1549_A966;
pub const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
pub const DURATION: u32 = 0x4489;
pub const TRACKS: u32 = 0x1654_AE6B;
pub const TRACK_ENTRY: u32 = 0xAE;
pub const TRACK_NUMBER: u32 = 0xD7;
pub const TRACK_TYPE: u32 = 0x83;
pub const DEFAULT_DURATION: u32 = 0x23_E383;
pub const CUES: u32 = 0x1C53_BB6B;
pub const CUE_POINT: u32 = 0xBB;
pub const CUE_TIME: u32 = 0xB3;
pub const CUE_TRACK_POSITIONS: u32 = 0xB7;
pub const CUE_TRACK: u32 = 0xF7;
pub const CUE_CLUSTER_POSITION: u32 = 0xF1;
pub const CLUSTER: u32 = 0x1F43_B675;
pub const TIMESTAMP: u32 = 0xE7;
pub const SIMPLE_BLOCK: u32 = 0xA3;
pub const BLOCK_GROUP: u32 = 0xA0;
pub const BLOCK: u32 = 0xA1;
pub const BLOCK_DURATION: u32 = 0x9B;
pub const REFERENCE_BLOCK: u32 = 0xFB;
pub const TAGS: u32 = 0x1254_C367;
pub const CHAPTERS: u32 = 0x1043_A770;
pub const ATTACHMENTS: u32 = 0x1941_A469;
pub const VOID: u32 = 0xEC;
pub const CRC32: u32 = 0xBF;

/// Elements that may appear directly in a Segment: inside a Cluster of unknown size, one of
/// them ends the Cluster.
pub fn is_top_level(id: u32) -> bool {
    matches!(
        id,
        SEEK_HEAD | INFO | TRACKS | CUES | CLUSTER | TAGS | CHAPTERS | ATTACHMENTS | EBML | SEGMENT
    )
}

/// An element header: where it starts, where its data starts, and its size (`None`: unknown).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub id: u32,
    pub start: u64,
    pub data: u64,
    pub size: Option<u64>,
}

impl Header {
    /// End of the data when the size is known.
    pub fn end(&self) -> Option<u64> {
        self.size.and_then(|s| self.data.checked_add(s))
    }
}

/// Decodes an element ID (1 to 4 bytes, the length marker kept) from the start of `b`.
pub fn decode_id(b: &[u8]) -> Result<(u32, usize), Error> {
    let first = *b.first().ok_or(Error::Truncated)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 4 {
        return Err(Error::invalid("element ID longer than 4 bytes"));
    }
    if b.len() < len {
        return Err(Error::Truncated);
    }
    let mut v = 0u32;
    for x in &b[..len] {
        v = (v << 8) | u32::from(*x);
    }
    let value_bits = 7 * len as u32;
    let value = v & ((1u32 << value_bits) - 1);
    if value == 0 || value == (1u32 << value_bits) - 1 {
        return Err(Error::invalid("reserved element ID"));
    }
    Ok((v, len))
}

/// Decodes an element size (1 to 8 bytes; all value bits set means unknown).
pub fn decode_size(b: &[u8]) -> Result<(Option<u64>, usize), Error> {
    let first = *b.first().ok_or(Error::Truncated)?;
    if first == 0 {
        return Err(Error::invalid("element size longer than 8 bytes"));
    }
    let len = first.leading_zeros() as usize + 1;
    if b.len() < len {
        return Err(Error::Truncated);
    }
    let mut v = u64::from(first) & (0xFF >> len);
    for x in &b[1..len] {
        v = (v << 8) | u64::from(*x);
    }
    let all = (1u64 << (7 * len as u32)) - 1;
    Ok((if v == all { None } else { Some(v) }, len))
}

/// Number of bytes of the shortest size encoding of `n`.
pub fn size_len(n: u64) -> usize {
    (1..=8).find(|l| n < (1u64 << (7 * l)) - 1).unwrap_or(9)
}

pub fn id_len(id: u32) -> usize {
    match id {
        0..=0xFF => 1,
        0x100..=0xFFFF => 2,
        0x1_0000..=0xFF_FFFF => 3,
        _ => 4,
    }
}

pub fn write_id(out: &mut Vec<u8>, id: u32) {
    let b = id.to_be_bytes();
    out.extend_from_slice(&b[4 - id_len(id)..]);
}

pub fn write_size(out: &mut Vec<u8>, n: u64) {
    let len = size_len(n);
    let marked = n | (1u64 << (7 * len));
    out.extend_from_slice(&marked.to_be_bytes()[8 - len..]);
}

/// An 8-byte size: the width does not depend on the value, so offsets can be computed first.
pub fn write_size8(out: &mut Vec<u8>, n: u64) {
    out.push(0x01);
    out.extend_from_slice(&n.to_be_bytes()[1..]);
}

/// Length of an element with `payload` bytes of data.
pub fn element_len(id: u32, payload: u64) -> u64 {
    id_len(id) as u64 + size_len(payload) as u64 + payload
}

pub fn uint_len(v: u64) -> usize {
    (8 - (v.leading_zeros() as usize / 8)).max(1)
}

pub fn write_element(out: &mut Vec<u8>, id: u32, payload: &[u8]) {
    write_id(out, id);
    write_size(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

pub fn write_uint(out: &mut Vec<u8>, id: u32, v: u64) {
    let len = uint_len(v);
    write_id(out, id);
    write_size(out, len as u64);
    out.extend_from_slice(&v.to_be_bytes()[8 - len..]);
}

/// An unsigned integer stored in 8 bytes.
pub fn write_uint8(out: &mut Vec<u8>, id: u32, v: u64) {
    write_id(out, id);
    write_size(out, 8);
    out.extend_from_slice(&v.to_be_bytes());
}

pub fn write_float(out: &mut Vec<u8>, id: u32, v: f64) {
    write_id(out, id);
    write_size(out, 8);
    out.extend_from_slice(&v.to_be_bytes());
}

pub fn read_uint(b: &[u8]) -> Result<u64, Error> {
    if b.len() > 8 {
        return Err(Error::invalid("unsigned integer longer than 8 bytes"));
    }
    Ok(b.iter().fold(0u64, |v, x| (v << 8) | u64::from(*x)))
}

pub fn read_float(b: &[u8]) -> Result<f64, Error> {
    match b.len() {
        0 => Ok(0.0),
        4 => Ok(f64::from(f32::from_be_bytes([b[0], b[1], b[2], b[3]]))),
        8 => {
            let mut a = [0u8; 8];
            a.copy_from_slice(b);
            Ok(f64::from_be_bytes(a))
        }
        _ => Err(Error::invalid("float of a size other than 0, 4 or 8")),
    }
}

/// Iterates the children of a master element held in memory; every child must have a known
/// size inside the buffer.
pub fn children(buf: &[u8]) -> impl Iterator<Item = Result<(u32, &[u8], &[u8]), Error>> {
    let mut pos = 0usize;
    std::iter::from_fn(move || {
        if pos >= buf.len() {
            return None;
        }
        let r = (|| {
            let (id, il) = decode_id(&buf[pos..])?;
            let (size, sl) = decode_size(&buf[pos + il..])?;
            let size =
                size.ok_or_else(|| Error::invalid("unknown size inside a header element"))?;
            let data = pos + il + sl;
            let end = usize::try_from(size)
                .ok()
                .and_then(|s| data.checked_add(s))
                .filter(|e| *e <= buf.len())
                .ok_or_else(|| Error::invalid("child element exceeds its parent"))?;
            let whole = &buf[pos..end];
            let payload = &buf[data..end];
            pos = end;
            Ok((id, whole, payload))
        })();
        if r.is_err() {
            pos = buf.len();
        }
        Some(r)
    })
}

/// A bounded reader over a seekable input of known length.
pub struct Reader<R> {
    inner: BufReader<R>,
    pos: u64,
    len: u64,
    elements: u64,
    limits: Limits,
}

impl<R: Read + Seek> Reader<R> {
    pub fn new(mut inner: R, limits: &Limits) -> Result<Self, Error> {
        let len = inner.seek(SeekFrom::End(0))?;
        if len > limits.max_input_bytes {
            return Err(Error::limit("input larger than the allowed size"));
        }
        inner.seek(SeekFrom::Start(0))?;
        Ok(Self {
            inner: BufReader::with_capacity(64 * 1024, inner),
            pos: 0,
            len,
            elements: 0,
            limits: limits.clone(),
        })
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn pos(&self) -> u64 {
        self.pos
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    pub fn seek_to(&mut self, pos: u64) -> Result<(), Error> {
        if pos > self.len {
            return Err(Error::Truncated);
        }
        let delta = pos as i128 - self.pos as i128;
        if let Ok(d) = i64::try_from(delta) {
            self.inner.seek_relative(d)?;
        } else {
            self.inner.seek(SeekFrom::Start(pos))?;
        }
        self.pos = pos;
        Ok(())
    }

    pub fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), Error> {
        if self.len - self.pos < buf.len() as u64 {
            return Err(Error::Truncated);
        }
        self.inner.read_exact(buf)?;
        self.pos += buf.len() as u64;
        Ok(())
    }

    /// Reads `n` bytes into a new buffer; the caller bounds `n`.
    pub fn read_vec(&mut self, n: u64) -> Result<Vec<u8>, Error> {
        if self.len - self.pos < n {
            return Err(Error::Truncated);
        }
        let mut v = vec![0u8; usize::try_from(n).map_err(|_| Error::limit("element too large"))?];
        self.read_exact(&mut v)?;
        Ok(v)
    }

    /// Reads the element header at the current position, counting it against the element
    /// limit and checking the deadline.
    pub fn header(&mut self) -> Result<Header, Error> {
        self.elements += 1;
        if self.elements > self.limits.max_elements {
            return Err(Error::limit("too many elements"));
        }
        if self.elements.is_multiple_of(1024) {
            self.limits.check_deadline()?;
        }
        let start = self.pos;
        let mut b = [0u8; 12];
        let n = (self.len - self.pos).min(12) as usize;
        if n < 2 {
            return Err(Error::Truncated);
        }
        self.inner.read_exact(&mut b[..n])?;
        self.pos += n as u64;
        let (id, il) = decode_id(&b[..n])?;
        let (size, sl) = decode_size(&b[il..n])?;
        let data = start + (il + sl) as u64;
        self.seek_to(data)?;
        Ok(Header {
            id,
            start,
            data,
            size,
        })
    }

    /// Copies `n` bytes from `from` to `out`.
    pub fn copy_to(&mut self, from: u64, n: u64, out: &mut impl Write) -> Result<(), Error> {
        self.seek_to(from)?;
        let mut left = n;
        let mut buf = vec![0u8; 64 * 1024];
        while left > 0 {
            self.limits.check_deadline()?;
            let k = left.min(buf.len() as u64) as usize;
            self.read_exact(&mut buf[..k])?;
            out.write_all(&buf[..k])?;
            left -= k as u64;
        }
        Ok(())
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            Error::Truncated
        } else {
            Error::Io(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_sizes() {
        assert_eq!(decode_id(&[0x1A, 0x45, 0xDF, 0xA3]).unwrap(), (EBML, 4));
        assert_eq!(decode_id(&[0xA3]).unwrap(), (SIMPLE_BLOCK, 1));
        assert!(decode_id(&[0xFF]).is_err());
        assert!(decode_id(&[0x80]).is_err());
        assert!(decode_id(&[0x08, 0, 0, 0, 1]).is_err());
        assert!(matches!(decode_id(&[0x1A, 0x45]), Err(Error::Truncated)));
        assert_eq!(decode_size(&[0x81]).unwrap(), (Some(1), 1));
        assert_eq!(decode_size(&[0xFF]).unwrap(), (None, 1));
        assert_eq!(
            decode_size(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]).unwrap(),
            (None, 8)
        );
        assert_eq!(decode_size(&[0x40, 0x02]).unwrap(), (Some(2), 2));
        assert!(decode_size(&[0x00, 1]).is_err());
        for n in [
            0u64,
            1,
            126,
            127,
            128,
            16_382,
            16_383,
            1 << 40,
            (1 << 56) - 2,
        ] {
            let mut v = Vec::new();
            write_size(&mut v, n);
            assert_eq!(v.len(), size_len(n), "{n}");
            assert_eq!(decode_size(&v).unwrap(), (Some(n), v.len()), "{n}");
            let mut w = Vec::new();
            write_size8(&mut w, n);
            assert_eq!(decode_size(&w).unwrap(), (Some(n), 8));
        }
        for id in [EBML, SEGMENT, CUES, TIMESTAMP_SCALE, DURATION, SIMPLE_BLOCK] {
            let mut v = Vec::new();
            write_id(&mut v, id);
            assert_eq!(decode_id(&v).unwrap(), (id, id_len(id)));
        }
        assert_eq!(uint_len(0), 1);
        assert_eq!(uint_len(255), 1);
        assert_eq!(uint_len(256), 2);
        assert_eq!(uint_len(u64::MAX), 8);
    }

    #[test]
    fn values() {
        let mut v = Vec::new();
        write_uint(&mut v, CUE_TIME, 300);
        assert_eq!(v, [0xB3, 0x82, 0x01, 0x2C]);
        assert_eq!(read_uint(&[0x01, 0x2C]).unwrap(), 300);
        assert!(read_uint(&[0; 9]).is_err());
        assert_eq!(read_float(&1.5f32.to_be_bytes()).unwrap(), 1.5);
        assert_eq!(read_float(&2.25f64.to_be_bytes()).unwrap(), 2.25);
        assert!(read_float(&[0; 3]).is_err());
        let mut m = Vec::new();
        write_uint(&mut m, TRACK_NUMBER, 1);
        write_uint(&mut m, TRACK_TYPE, 1);
        let kids: Vec<_> = children(&m).collect::<Result<_, _>>().unwrap();
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[1].0, TRACK_TYPE);
        assert!(children(&m[..m.len() - 1]).any(|r| r.is_err()));
    }
}
