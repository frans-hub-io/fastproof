//! Decoding of import files: UTF-8 (with or without a byte order mark) or UTF-16 LE/BE with
//! a byte order mark, transcoded to UTF-8 on the fly, with a cap on the bytes read.

use std::io::{self, BufRead, BufReader, Read};

use crate::ImportError;

/// A reader that fails after `max` bytes.
struct Capped<R> {
    inner: R,
    left: u64,
    max: u64,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            let mut probe = [0u8; 1];
            if self.inner.read(&mut probe)? == 0 {
                return Ok(0);
            }
            return Err(io::Error::other(LimitHit(self.max)));
        }
        let want = buf.len().min(self.left.min(usize::MAX as u64) as usize);
        let n = self.inner.read(&mut buf[..want])?;
        self.left -= n as u64;
        Ok(n)
    }
}

#[derive(Debug)]
struct LimitHit(u64);

impl std::fmt::Display for LimitHit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the file is larger than {} bytes", self.0)
    }
}

impl std::error::Error for LimitHit {}

/// Turns an I/O error of a decoded reader into the import error (a hit cap is a limit).
pub fn io_error(e: io::Error) -> ImportError {
    if let Some(l) = e.get_ref().and_then(|i| i.downcast_ref::<LimitHit>()) {
        return ImportError::limit("maxImportBytes", l.0);
    }
    if e.kind() == io::ErrorKind::InvalidData {
        return ImportError::invalid(format!("the file is not valid text: {e}"));
    }
    ImportError::from(e)
}

/// UTF-16 to UTF-8.
struct Utf16<R> {
    inner: R,
    big_endian: bool,
    out: Vec<u8>,
    pos: usize,
    pending: Option<u16>,
    done: bool,
}

impl<R: Read> Utf16<R> {
    fn unit(&mut self) -> io::Result<Option<u16>> {
        let mut b = [0u8; 2];
        let mut n = 0;
        while n < 2 {
            let r = self.inner.read(&mut b[n..])?;
            if r == 0 {
                if n == 0 {
                    return Ok(None);
                }
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "odd number of bytes in UTF-16 text",
                ));
            }
            n += r;
        }
        Ok(Some(if self.big_endian {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        }))
    }

    fn fill(&mut self) -> io::Result<()> {
        self.out.clear();
        self.pos = 0;
        while self.out.len() < 8192 {
            let u = match self.pending.take() {
                Some(u) => u,
                None => match self.unit()? {
                    Some(u) => u,
                    None => {
                        self.done = true;
                        return Ok(());
                    }
                },
            };
            let c = if (0xD800..0xDC00).contains(&u) {
                match self.unit()? {
                    Some(lo) if (0xDC00..0xE000).contains(&lo) => char::from_u32(
                        0x10000 + (((u as u32) - 0xD800) << 10) + ((lo as u32) - 0xDC00),
                    ),
                    Some(other) => {
                        self.pending = Some(other);
                        None
                    }
                    None => None,
                }
            } else {
                char::from_u32(u as u32)
            };
            let c = c.unwrap_or(char::REPLACEMENT_CHARACTER);
            let mut tmp = [0u8; 4];
            self.out
                .extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
        }
        Ok(())
    }
}

impl<R: Read> Read for Utf16<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.out.len() {
            if self.done {
                return Ok(0);
            }
            self.fill()?;
            if self.out.is_empty() {
                return Ok(0);
            }
        }
        let n = buf.len().min(self.out.len() - self.pos);
        buf[..n].copy_from_slice(&self.out[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Opens a file as UTF-8 text: the byte order mark is removed, UTF-16 is transcoded, and
/// reading more than `max_bytes` (of the original file) fails with `maxImportBytes`.
pub fn decode<'a>(
    input: impl Read + 'a,
    max_bytes: u64,
) -> Result<Box<dyn BufRead + 'a>, ImportError> {
    let mut r = BufReader::with_capacity(
        64 * 1024,
        Capped {
            inner: input,
            left: max_bytes,
            max: max_bytes,
        },
    );
    let head = r.fill_buf().map_err(io_error)?;
    if head.starts_with(&[0xEF, 0xBB, 0xBF]) {
        r.consume(3);
        return Ok(Box::new(r));
    }
    let be = head.starts_with(&[0xFE, 0xFF]);
    let le = head.starts_with(&[0xFF, 0xFE]);
    if be || le {
        r.consume(2);
        return Ok(Box::new(BufReader::with_capacity(
            64 * 1024,
            Utf16 {
                inner: r,
                big_endian: be,
                out: Vec::new(),
                pos: 0,
                pending: None,
                done: false,
            },
        )));
    }
    Ok(Box::new(r))
}

/// Decodes a prefix of a file (preview): like [`decode`] but lossy and without a cap.
pub fn decode_prefix(bytes: &[u8]) -> String {
    let mut out = Vec::new();
    match decode(bytes, u64::MAX) {
        Ok(mut r) => {
            let _ = r.read_to_end(&mut out);
        }
        Err(_) => out.extend_from_slice(bytes),
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(b: &[u8], max: u64) -> Result<String, ImportError> {
        let mut r = decode(b, max)?;
        let mut s = String::new();
        r.read_to_string(&mut s).map_err(io_error)?;
        Ok(s)
    }

    #[test]
    fn boms_and_utf16() {
        assert_eq!(all(b"\xEF\xBB\xBFa,b", 100).unwrap(), "a,b");
        assert_eq!(all(b"a,b", 100).unwrap(), "a,b");
        let mut le = vec![0xFF, 0xFE];
        for u in "Тест,😀".encode_utf16() {
            le.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(all(&le, 100).unwrap(), "Тест,😀");
        let mut be = vec![0xFE, 0xFF];
        for u in "ab".encode_utf16() {
            be.extend_from_slice(&u.to_be_bytes());
        }
        assert_eq!(all(&be, 100).unwrap(), "ab");
        assert!(matches!(
            all(b"0123456789", 5),
            Err(ImportError::Limit {
                limit: "maxImportBytes",
                ..
            })
        ));
        assert_eq!(all(b"01234", 5).unwrap(), "01234");
    }
}
