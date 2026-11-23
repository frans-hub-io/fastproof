//! Makes stored test videos seekable in browsers without re-encoding them.
//!
//! - WebM/Matroska ([`webm`]): recorders that stop without finalising the file (MediaRecorder,
//!   interrupted muxers) leave no `Cues`, no `Duration` and elements of unknown size, so a
//!   browser shows an unknown duration and can only seek into what it has buffered. The file is
//!   rewritten as `SeekHead`, `Info` with `Duration`, `Tracks`, the other header elements,
//!   `Cues` with one point per cluster that holds a keyframe, then the clusters with their
//!   bodies copied byte for byte.
//! - MP4/MOV ([`mp4`]): when `moov` follows the media data, it is moved in front of it and the
//!   chunk offsets (`stco`/`co64`) are shifted ("faststart").
//!
//! Everything is bounded by [`Limits`]: input size, bytes held in memory, element count and a
//! deadline. The input is read twice from a seekable file; the output is written to another.
//! Each result is parsed again before it is accepted.

use std::fmt;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::time::Instant;

pub mod ebml;
pub mod mp4;
pub mod webm;

/// Why preparing failed.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The input ends inside an element that must be complete.
    Truncated,
    /// The input breaks the container format.
    Invalid(String),
    /// A bound of [`Limits`] was reached.
    Limit(String),
    /// The deadline passed.
    Timeout,
}

impl Error {
    pub fn invalid(m: impl Into<String>) -> Self {
        Error::Invalid(m.into())
    }

    pub fn limit(m: impl Into<String>) -> Self {
        Error::Limit(m.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "i/o error: {e}"),
            Error::Truncated => f.write_str("the file is truncated"),
            Error::Invalid(m) => write!(f, "invalid file: {m}"),
            Error::Limit(m) => write!(f, "limit reached: {m}"),
            Error::Timeout => f.write_str("time limit reached"),
        }
    }
}

impl std::error::Error for Error {}

/// Bounds of one preparation.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Largest input accepted.
    pub max_input_bytes: u64,
    /// Largest header element (`Info`, `Tracks`, `SeekHead`, MP4 `moov`) read into memory.
    pub max_header_bytes: u64,
    /// Bytes held in memory for headers, the cluster index and cue points together.
    pub max_memory_bytes: u64,
    /// Elements (or boxes) read from the input, over both passes.
    pub max_elements: u64,
    /// Work stops with [`Error::Timeout`] after this instant.
    pub deadline: Option<Instant>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 4 << 30,
            max_header_bytes: 4 << 20,
            max_memory_bytes: 64 << 20,
            max_elements: 50_000_000,
            deadline: None,
        }
    }
}

impl Limits {
    pub fn check_deadline(&self) -> Result<(), Error> {
        match self.deadline {
            Some(d) if Instant::now() >= d => Err(Error::Timeout),
            _ => Ok(()),
        }
    }
}

/// Bytes charged against [`Limits::max_memory_bytes`].
#[derive(Debug)]
pub struct Budget {
    used: u64,
    cap: u64,
}

impl Budget {
    pub fn new(cap: u64) -> Self {
        Self { used: 0, cap }
    }

    pub fn charge(&mut self, n: u64) -> Result<(), Error> {
        self.used = self.used.saturating_add(n);
        if self.used > self.cap {
            return Err(Error::limit("memory budget exhausted"));
        }
        Ok(())
    }

    pub fn used(&self) -> u64 {
        self.used
    }
}

/// Container formats handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Webm,
    Mp4,
}

impl Format {
    /// File extension of a prepared copy.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Webm => "webm",
            Format::Mp4 => "mp4",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Format::Webm => "webm",
            Format::Mp4 => "mp4",
        }
    }
}

/// What [`prepare`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A seekable copy of `bytes` bytes was written to the output path.
    Prepared { format: Format, bytes: u64 },
    /// The input already seeks well; nothing was written.
    AlreadySeekable(Format),
    /// Not a container this crate handles, or one it leaves alone (the reason).
    Unsupported(String),
}

/// Detects the container from the first bytes.
pub fn detect(head: &[u8]) -> Option<Format> {
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some(Format::Webm);
    }
    if head.len() >= 8 && mp4::is_top_box_type(&head[4..8]) {
        return Some(Format::Mp4);
    }
    None
}

/// Writes a seekable copy of `input` to `output` when the input needs one, and checks the copy
/// by parsing it again. The output file exists afterwards only for [`Outcome::Prepared`].
pub fn prepare(input: &Path, output: &Path, limits: &Limits) -> Result<Outcome, Error> {
    let r = (|| {
        let mut f = File::open(input)?;
        let mut head = [0u8; 12];
        let n = read_up_to(&mut f, &mut head)?;
        drop(f);
        match detect(&head[..n]) {
            Some(Format::Webm) => webm::prepare_file(input, output, limits),
            Some(Format::Mp4) => mp4::prepare_file(input, output, limits),
            None => Ok(Outcome::Unsupported("not a WebM or MP4 file".into())),
        }
    })();
    if !matches!(r, Ok(Outcome::Prepared { .. })) {
        let _ = std::fs::remove_file(output);
    }
    r
}

/// Creates `output`, lets `write` fill it and syncs it; returns its size.
pub(crate) fn write_file(
    output: &Path,
    write: impl FnOnce(&mut BufWriter<File>) -> Result<(), Error>,
) -> Result<u64, Error> {
    let mut w = BufWriter::with_capacity(256 * 1024, File::create(output)?);
    write(&mut w)?;
    w.flush()?;
    let file = w.into_inner().map_err(|e| Error::Io(e.into_error()))?;
    file.sync_data()?;
    Ok(file.metadata()?.len())
}

fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}
