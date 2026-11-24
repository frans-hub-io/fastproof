//! A streaming `multipart/form-data` reader with hard limits: the whole body, the number of
//! parts and the size of each part's header block are bounded, and part data is handed out
//! in chunks as it arrives (nothing is read into memory as a whole). Searching for the
//! boundary is linear (`memchr::memmem`).

use bytes::{Bytes, BytesMut};
use futures::{Stream, StreamExt};

/// Bounds of one multipart body.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Parts, at most.
    pub max_parts: usize,
    /// Bytes of one part's header block (and of the preamble), at most.
    pub max_header_bytes: usize,
    /// Bytes of the whole body, at most.
    pub max_body_bytes: u64,
}

/// Why a body was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The body is larger than [`Limits::max_body_bytes`].
    TooLarge(u64),
    /// More parts than [`Limits::max_parts`].
    TooManyParts(usize),
    /// A header block longer than [`Limits::max_header_bytes`].
    HeaderTooLarge(usize),
    /// Not a well-formed multipart body.
    Malformed(&'static str),
    /// Reading the body failed.
    Body(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooLarge(n) => write!(f, "the body exceeds {n} bytes"),
            Error::TooManyParts(n) => write!(f, "more than {n} parts"),
            Error::HeaderTooLarge(n) => write!(f, "a part header exceeds {n} bytes"),
            Error::Malformed(m) => write!(f, "malformed multipart body: {m}"),
            Error::Body(m) => write!(f, "reading the body failed: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// The header of a part.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PartHead {
    /// `name` of `Content-Disposition`.
    pub name: Option<String>,
    /// `filename` of `Content-Disposition` as sent (a matching key, not a path).
    pub filename: Option<String>,
    pub content_type: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Start,
    InPart,
    AfterDelimiter,
    Done,
}

/// The boundary of a `multipart/*` content type.
pub fn boundary(content_type: &str) -> Option<String> {
    let mut parts = content_type.split(';');
    let mime = parts.next()?.trim().to_ascii_lowercase();
    if !mime.starts_with("multipart/") {
        return None;
    }
    for p in parts {
        let (k, v) = p.split_once('=')?;
        if k.trim().eq_ignore_ascii_case("boundary") {
            let v = v.trim().trim_matches('"');
            if v.is_empty() || v.len() > 70 {
                return None;
            }
            return Some(v.to_string());
        }
    }
    None
}

/// Reads parts from a body stream.
pub struct Reader<S> {
    stream: S,
    buf: BytesMut,
    delimiter: Vec<u8>,
    state: State,
    total: u64,
    parts: usize,
    eof: bool,
    limits: Limits,
}

impl<S, E> Reader<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    pub fn new(stream: S, boundary: &str, limits: Limits) -> Self {
        let mut buf = BytesMut::with_capacity(16 * 1024);
        // The first boundary may start the body; a virtual line break before it lets one
        // delimiter (`\r\n--boundary`) find every boundary.
        buf.extend_from_slice(b"\r\n");
        Self {
            stream,
            buf,
            delimiter: format!("\r\n--{boundary}").into_bytes(),
            state: State::Start,
            total: 0,
            parts: 0,
            eof: false,
            limits,
        }
    }

    /// Bytes of the body read so far.
    pub fn bytes_read(&self) -> u64 {
        self.total
    }

    async fn fill(&mut self) -> Result<bool, Error> {
        if self.eof {
            return Ok(false);
        }
        match self.stream.next().await {
            None => {
                self.eof = true;
                Ok(false)
            }
            Some(Err(e)) => Err(Error::Body(e.to_string())),
            Some(Ok(b)) => {
                self.total += b.len() as u64;
                if self.total > self.limits.max_body_bytes {
                    return Err(Error::TooLarge(self.limits.max_body_bytes));
                }
                self.buf.extend_from_slice(&b);
                Ok(true)
            }
        }
    }

    fn find(&self, from: usize) -> Option<usize> {
        memchr::memmem::find(&self.buf[from..], &self.delimiter).map(|i| i + from)
    }

    /// The next part's header, after skipping what is left of the current part; `None` after
    /// the closing boundary.
    pub async fn next_part(&mut self) -> Result<Option<PartHead>, Error> {
        loop {
            match self.state {
                State::Done => return Ok(None),
                State::InPart => while self.chunk().await?.is_some() {},
                State::Start => {
                    let mut from = 0;
                    loop {
                        if let Some(i) = self.find(from) {
                            let _ = self.buf.split_to(i + self.delimiter.len());
                            self.state = State::AfterDelimiter;
                            break;
                        }
                        if self.buf.len() > self.limits.max_header_bytes + self.delimiter.len() {
                            return Err(Error::Malformed("no boundary at the start"));
                        }
                        from = self.buf.len().saturating_sub(self.delimiter.len());
                        if !self.fill().await? {
                            return Err(Error::Malformed("no boundary"));
                        }
                    }
                }
                State::AfterDelimiter => return self.after_delimiter().await,
            }
        }
    }

    async fn after_delimiter(&mut self) -> Result<Option<PartHead>, Error> {
        while self.buf.len() < 2 {
            if !self.fill().await? {
                return Err(Error::Malformed("unexpected end after a boundary"));
            }
        }
        if &self.buf[..2] == b"--" {
            self.state = State::Done;
            return Ok(None);
        }
        // Transport padding: spaces and tabs before the line break.
        loop {
            let pad = self
                .buf
                .iter()
                .take_while(|b| matches!(b, b' ' | b'\t'))
                .count();
            if pad > 256 {
                return Err(Error::Malformed("padding after a boundary"));
            }
            if self.buf.len() >= pad + 2 {
                if &self.buf[pad..pad + 2] != b"\r\n" {
                    return Err(Error::Malformed("no line break after a boundary"));
                }
                let _ = self.buf.split_to(pad + 2);
                break;
            }
            if !self.fill().await? {
                return Err(Error::Malformed("unexpected end after a boundary"));
            }
        }
        self.parts += 1;
        if self.parts > self.limits.max_parts {
            return Err(Error::TooManyParts(self.limits.max_parts));
        }
        let head = loop {
            if self.buf.starts_with(b"\r\n") {
                let _ = self.buf.split_to(2);
                break PartHead::default();
            }
            if let Some(i) = memchr::memmem::find(&self.buf, b"\r\n\r\n") {
                if i > self.limits.max_header_bytes {
                    return Err(Error::HeaderTooLarge(self.limits.max_header_bytes));
                }
                let raw = self.buf.split_to(i + 4);
                break parse_head(&raw[..i]);
            }
            if self.buf.len() > self.limits.max_header_bytes + 4 {
                return Err(Error::HeaderTooLarge(self.limits.max_header_bytes));
            }
            if !self.fill().await? {
                return Err(Error::Malformed("unexpected end in a part header"));
            }
        };
        self.state = State::InPart;
        Ok(Some(head))
    }

    /// The next chunk of the current part's data; `None` at its end.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, Error> {
        if self.state != State::InPart {
            return Ok(None);
        }
        loop {
            if let Some(i) = self.find(0) {
                let data = self.buf.split_to(i).freeze();
                let _ = self.buf.split_to(self.delimiter.len());
                self.state = State::AfterDelimiter;
                return Ok((!data.is_empty()).then_some(data));
            }
            let keep = self.delimiter.len() - 1;
            if self.buf.len() > keep {
                let n = self.buf.len() - keep;
                return Ok(Some(self.buf.split_to(n).freeze()));
            }
            if !self.fill().await? {
                return Err(Error::Malformed("unexpected end in a part"));
            }
        }
    }
}

fn parse_head(raw: &[u8]) -> PartHead {
    let text = String::from_utf8_lossy(raw);
    let mut head = PartHead::default();
    for line in text.split("\r\n") {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let k = k.trim();
        if k.eq_ignore_ascii_case("content-type") {
            head.content_type = Some(v.trim().to_string());
        } else if k.eq_ignore_ascii_case("content-disposition") {
            let mut star = None;
            for (pk, pv) in params(v) {
                match pk.to_ascii_lowercase().as_str() {
                    "name" => head.name = Some(pv),
                    "filename" => head.filename = Some(pv),
                    "filename*" => star = ext_value(&pv),
                    _ => {}
                }
            }
            if head.filename.is_none() {
                head.filename = star;
            }
        }
    }
    head
}

/// `key=value` parameters of a header value; values may be quoted with `\` escapes.
fn params(v: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let b = v.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i] == b';' || b[i].is_ascii_whitespace()) {
            i += 1;
        }
        let ks = i;
        while i < b.len() && b[i] != b'=' && b[i] != b';' {
            i += 1;
        }
        let key = v[ks..i].trim().to_string();
        if i >= b.len() || b[i] == b';' {
            continue;
        }
        i += 1;
        while i < b.len() && b[i] == b' ' {
            i += 1;
        }
        let mut val = Vec::new();
        if i < b.len() && b[i] == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' && i + 1 < b.len() {
                    i += 1;
                }
                val.push(b[i]);
                i += 1;
            }
            i += 1;
        } else {
            while i < b.len() && b[i] != b';' {
                val.push(b[i]);
                i += 1;
            }
        }
        out.push((key, String::from_utf8_lossy(&val).trim_end().to_string()));
    }
    out
}

/// An RFC 5987 value (`UTF-8''%E2%82%AC.png`).
fn ext_value(v: &str) -> Option<String> {
    let (_, enc) = v.split_once("''")?;
    let b = enc.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            let h = std::str::from_utf8(&b[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        max_parts: 10,
        max_header_bytes: 8192,
        max_body_bytes: 1 << 20,
    };

    fn stream(
        body: Vec<u8>,
        chunk: usize,
    ) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Unpin {
        let chunks: Vec<Result<Bytes, std::io::Error>> = body
            .chunks(chunk.max(1))
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        futures::stream::iter(chunks)
    }

    fn body(parts: &[(&str, Option<&str>, &str, &[u8])]) -> Vec<u8> {
        let mut b = Vec::new();
        for (name, file, ct, data) in parts {
            b.extend_from_slice(b"--XyZ\r\n");
            match file {
                Some(f) => b.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\n"
                    )
                    .as_bytes(),
                ),
                None => b.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"\r\n").as_bytes(),
                ),
            }
            b.extend_from_slice(format!("Content-Type: {ct}\r\n\r\n").as_bytes());
            b.extend_from_slice(data);
            b.extend_from_slice(b"\r\n");
        }
        b.extend_from_slice(b"--XyZ--\r\n");
        b
    }

    async fn read_all<S, E>(r: &mut Reader<S>) -> Result<Vec<(PartHead, Vec<u8>)>, Error>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        let mut out = Vec::new();
        while let Some(h) = r.next_part().await? {
            let mut data = Vec::new();
            while let Some(c) = r.chunk().await? {
                data.extend_from_slice(&c);
            }
            out.push((h, data));
        }
        Ok(out)
    }

    #[tokio::test]
    async fn reads_parts_in_any_chunking() {
        let tricky: Vec<u8> = b"\r\n--XyX\r\n--Xy\r\n\r\n--\r".repeat(50);
        let b = body(&[
            (
                "json_request_part",
                None,
                "application/json",
                b"[{\"a\":1}]",
            ),
            ("file", Some("shot.png"), "image/png", &tricky),
            ("file", Some("empty.txt"), "text/plain", b""),
        ]);
        for chunk in [1, 2, 3, 7, 64, 100_000] {
            let mut r = Reader::new(stream(b.clone(), chunk), "XyZ", LIMITS);
            let parts = read_all(&mut r).await.unwrap();
            assert_eq!(parts.len(), 3, "chunk {chunk}");
            assert_eq!(parts[0].0.name.as_deref(), Some("json_request_part"));
            assert_eq!(parts[0].1, b"[{\"a\":1}]");
            assert_eq!(parts[1].0.filename.as_deref(), Some("shot.png"));
            assert_eq!(parts[1].0.content_type.as_deref(), Some("image/png"));
            assert_eq!(parts[1].1, tricky);
            assert_eq!(parts[2].1, b"");
        }
    }

    #[tokio::test]
    async fn skipping_parts_and_preambles() {
        let mut b = b"preamble text\r\n".to_vec();
        b.extend(body(&[
            ("a", None, "text/plain", b"one"),
            ("b", None, "text/plain", b"two"),
        ]));
        let mut r = Reader::new(stream(b, 5), "XyZ", LIMITS);
        assert_eq!(
            r.next_part().await.unwrap().unwrap().name.as_deref(),
            Some("a")
        );
        let h = r.next_part().await.unwrap().unwrap();
        assert_eq!(h.name.as_deref(), Some("b"));
        assert_eq!(
            r.chunk().await.unwrap().unwrap(),
            Bytes::from_static(b"two")
        );
        assert_eq!(r.next_part().await.unwrap(), None);
    }

    #[tokio::test]
    async fn limits() {
        let many: Vec<(&str, Option<&str>, &str, &[u8])> = (0..11)
            .map(|_| ("f", None, "text/plain", &b"x"[..]))
            .collect();
        let mut r = Reader::new(stream(body(&many), 64), "XyZ", LIMITS);
        assert_eq!(read_all(&mut r).await.unwrap_err(), Error::TooManyParts(10));

        let big = vec![b'a'; 2 << 20];
        let mut r = Reader::new(
            stream(body(&[("f", None, "text/plain", &big)]), 4096),
            "XyZ",
            LIMITS,
        );
        assert_eq!(
            read_all(&mut r).await.unwrap_err(),
            Error::TooLarge(1 << 20)
        );

        let mut b = b"--XyZ\r\nX-Long: ".to_vec();
        b.extend(vec![b'h'; 9000]);
        b.extend_from_slice(b"\r\n\r\ndata\r\n--XyZ--");
        let mut r = Reader::new(stream(b, 512), "XyZ", LIMITS);
        assert_eq!(
            read_all(&mut r).await.unwrap_err(),
            Error::HeaderTooLarge(8192)
        );

        let mut r = Reader::new(stream(b"no boundary here".to_vec(), 4), "XyZ", LIMITS);
        assert!(matches!(
            read_all(&mut r).await.unwrap_err(),
            Error::Malformed(_)
        ));

        let mut b = body(&[("a", None, "text/plain", b"one")]);
        b.truncate(b.len() - 12);
        let mut r = Reader::new(stream(b, 4), "XyZ", LIMITS);
        assert!(matches!(
            read_all(&mut r).await.unwrap_err(),
            Error::Malformed(_)
        ));
    }

    #[test]
    fn boundaries_and_dispositions() {
        assert_eq!(
            boundary("multipart/form-data; boundary=\"abc\"").as_deref(),
            Some("abc")
        );
        assert_eq!(
            boundary("Multipart/Mixed;charset=utf-8; BOUNDARY=x-1").as_deref(),
            Some("x-1")
        );
        assert_eq!(boundary("application/json"), None);
        assert_eq!(boundary("multipart/form-data"), None);
        let h = parse_head(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"a\\\"b;c.png\"\r\ncontent-type: image/png",
        );
        assert_eq!(h.name.as_deref(), Some("file"));
        assert_eq!(h.filename.as_deref(), Some("a\"b;c.png"));
        assert_eq!(h.content_type.as_deref(), Some("image/png"));
        let h =
            parse_head(b"Content-Disposition: form-data; name=f; filename*=UTF-8''%E2%82%AC.png");
        assert_eq!(h.filename.as_deref(), Some("\u{20ac}.png"));
    }
}
