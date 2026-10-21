//! Hand written HTTP/1.1 subset: request head parsing, response writing.

use crate::util::{percent_decode_path, percent_decode_query};
use std::fs::File;
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};

pub const MAX_HEAD_BYTES: usize = 32 * 1024;
pub const MAX_HEADERS: usize = 100;

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub target: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub version: String,
    pub headers: Vec<(String, String)>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn q(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn cookie(&self, name: &str) -> Option<String> {
        let raw = self.header("cookie")?;
        for part in raw.split(';') {
            let part = part.trim();
            if let Some((k, v)) = part.split_once('=') {
                if k.trim() == name {
                    return Some(v.trim().to_string());
                }
            }
        }
        None
    }

    pub fn content_length(&self) -> Option<u64> {
        self.header("content-length")
            .and_then(|v| v.trim().parse::<u64>().ok())
    }

    /// HTTP/1.1 keeps the connection open unless `Connection: close` is sent;
    /// HTTP/1.0 does the opposite and needs an explicit `keep-alive` token.
    pub fn wants_keep_alive(&self) -> bool {
        let mut keep = !self.version.eq_ignore_ascii_case("HTTP/1.0");
        if let Some(value) = self.header("connection") {
            for token in value.split(',') {
                let token = token.trim();
                if token.eq_ignore_ascii_case("close") {
                    keep = false;
                } else if token.eq_ignore_ascii_case("keep-alive") {
                    keep = true;
                }
            }
        }
        keep
    }
}

/// Reads one request head. `Ok(None)` means the peer closed cleanly.
pub fn read_head<R: BufRead>(reader: &mut R) -> io::Result<Option<Request>> {
    let mut first = String::new();
    let mut total = 0usize;
    loop {
        first.clear();
        let n = read_line(reader, &mut first, MAX_HEAD_BYTES)?;
        if n == 0 {
            return Ok(None);
        }
        total += n;
        if !first.trim().is_empty() {
            break;
        }
        if total > MAX_HEAD_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "header too large"));
        }
    }

    let line = first.trim_end_matches(['\r', '\n']);
    let mut parts = line.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let version = parts.next().unwrap_or("HTTP/1.0").to_string();
    if method.is_empty() || target.is_empty() || !version.starts_with("HTTP/") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "malformed request line",
        ));
    }

    let mut headers = Vec::with_capacity(16);
    loop {
        let mut line = String::new();
        let n = read_line(reader, &mut line, MAX_HEAD_BYTES)?;
        if n == 0 {
            break;
        }
        total += n;
        if total > MAX_HEAD_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "header too large"));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "too many headers"));
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            let value = v.trim();
            let name = k.trim().to_ascii_lowercase();
            if name.is_empty() || value.len() > 8192 || value.chars().any(|c| c == '\r' || c == '\n')
            {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "bad header"));
            }
            headers.push((name, value.to_string()));
        } else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad header"));
        }
    }

    let (raw_path, raw_query) = match target.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (target.as_str(), None),
    };
    let path = percent_decode_path(raw_path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad percent encoding"))?;
    let mut query = Vec::new();
    if let Some(q) = raw_query {
        for pair in q.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (k, v) = match pair.split_once('=') {
                Some((k, v)) => (k, v),
                None => (pair, ""),
            };
            if let (Some(k), Some(v)) = (percent_decode_query(k), percent_decode_query(v)) {
                query.push((k, v));
            }
        }
    }

    Ok(Some(Request {
        method,
        target,
        path,
        query,
        version,
        headers,
    }))
}

fn read_line<R: BufRead>(reader: &mut R, out: &mut String, limit: usize) -> io::Result<usize> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                buf.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
                if buf.len() > limit {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    if buf.is_empty() {
        return Ok(0);
    }
    let n = buf.len();
    out.push_str(&String::from_utf8_lossy(&buf));
    Ok(n)
}

pub enum Body {
    Empty,
    Bytes(Vec<u8>),
    File { file: File, offset: u64, len: u64 },
    /// Generated on the fly (folder downloads); `len` must be exact.
    Stream { reader: Box<dyn Read>, len: u64 },
}

impl Body {
    pub fn len(&self) -> Option<u64> {
        match self {
            Body::Empty => Some(0),
            Body::Bytes(b) => Some(b.len() as u64),
            Body::File { len, .. } => Some(*len),
            Body::Stream { len, .. } => Some(*len),
        }
    }
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl Response {
    pub fn new(status: u16) -> Self {
        Response {
            status,
            headers: Vec::new(),
            body: Body::Empty,
        }
    }

    pub fn header(mut self, k: &str, v: impl Into<String>) -> Self {
        self.headers.push((k.to_string(), v.into()));
        self
    }

    pub fn with_body(mut self, bytes: Vec<u8>) -> Self {
        self.body = Body::Bytes(bytes);
        self
    }

    pub fn with_stream(mut self, reader: Box<dyn Read>, len: u64) -> Self {
        self.body = Body::Stream { reader, len };
        self
    }

    pub fn json(status: u16, text: String) -> Self {
        Response::new(status)
            .header("Content-Type", "application/json; charset=utf-8")
            .header("Cache-Control", "no-store")
            .with_body(text.into_bytes())
    }

    pub fn error(status: u16, code: &str, message: &str) -> Self {
        let body = crate::json::obj(vec![
            ("error", crate::json::Json::Str(code.to_string())),
            ("message", crate::json::Json::Str(message.to_string())),
        ])
        .dump();
        Response::json(status, body)
    }

    pub fn has_header(&self, name: &str) -> bool {
        self.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name))
    }
}

pub fn status_text(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        206 => "Partial Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Payload Too Large",
        416 => "Range Not Satisfiable",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        _ => "Unknown",
    }
}

pub fn write_response<W: Write>(
    writer: &mut W,
    method: &str,
    mut resp: Response,
    keep_alive: bool,
) -> io::Result<()> {
    let body_len = resp.body.len().unwrap_or(0);
    if !resp.has_header("Content-Length") && !matches!(resp.status, 204 | 304) {
        resp.headers
            .push(("Content-Length".into(), body_len.to_string()));
    }
    if !resp.has_header("Date") {
        resp.headers
            .push(("Date".into(), crate::util::http_date(crate::util::now_secs())));
    }
    if !resp.has_header("Server") {
        resp.headers.push((
            "Server".into(),
            concat!("fern/", env!("CARGO_PKG_VERSION")).into(),
        ));
    }
    if !resp.has_header("Connection") {
        resp.headers.push((
            "Connection".into(),
            if keep_alive { "keep-alive" } else { "close" }.into(),
        ));
    }
    if keep_alive && !resp.has_header("Keep-Alive") {
        resp.headers
            .push(("Keep-Alive".into(), "timeout=120, max=100".into()));
    }
    if !resp.has_header("X-Content-Type-Options") {
        resp.headers
            .push(("X-Content-Type-Options".into(), "nosniff".into()));
    }

    let mut head = String::with_capacity(256);
    head.push_str("HTTP/1.1 ");
    head.push_str(&resp.status.to_string());
    head.push(' ');
    head.push_str(status_text(resp.status));
    head.push_str("\r\n");
    for (k, v) in &resp.headers {
        head.push_str(k);
        head.push_str(": ");
        head.push_str(v);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    writer.write_all(head.as_bytes())?;

    if method.eq_ignore_ascii_case("HEAD") || matches!(resp.status, 204 | 304) {
        writer.flush()?;
        return Ok(());
    }

    match resp.body {
        Body::Empty => {}
        Body::Bytes(bytes) => writer.write_all(&bytes)?,
        Body::File {
            mut file,
            offset,
            len,
        } => {
            if offset > 0 {
                file.seek(SeekFrom::Start(offset))?;
            }
            let mut taken = file.take(len);
            copy_exactly(&mut taken, writer)?;
        }
        Body::Stream { mut reader, len } => {
            let mut taken = (&mut reader).take(len);
            copy_exactly(&mut taken, writer)?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn copy_exactly<R: Read, W: Write>(reader: &mut R, writer: &mut W) -> io::Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
    }
    Ok(())
}

pub fn write_continue<W: Write>(writer: &mut W) -> io::Result<()> {
    writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn parses_request_with_query() {
        let raw = b"GET /a%20b/file.txt?path=%2F&x=1+2 HTTP/1.1\r\nHost: x\r\nCookie: u=abc; z=1\r\n\r\n";
        let mut r = BufReader::new(Cursor::new(&raw[..]));
        let req = read_head(&mut r).unwrap().unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/a b/file.txt");
        assert_eq!(req.q("path"), Some("/"));
        assert_eq!(req.q("x"), Some("1 2"));
        assert_eq!(req.cookie("u").as_deref(), Some("abc"));
    }

    #[test]
    fn rejects_bad_request_line() {
        let raw = b"BADLINE\r\n\r\n";
        let mut r = BufReader::new(Cursor::new(&raw[..]));
        assert!(read_head(&mut r).is_err());
    }

    #[test]
    fn eof_returns_none() {
        let mut r = BufReader::new(Cursor::new(&b""[..]));
        assert!(read_head(&mut r).unwrap().is_none());
    }

    #[test]
    fn keep_alive_follows_version_and_connection_header() {
        let keep = |raw: &'static [u8]| {
            let mut r = BufReader::new(Cursor::new(raw));
            read_head(&mut r).unwrap().unwrap().wants_keep_alive()
        };
        assert!(keep(b"GET / HTTP/1.1\r\n\r\n"));
        assert!(!keep(b"GET / HTTP/1.0\r\n\r\n"));
        assert!(!keep(b"GET / HTTP/1.1\r\nConnection: close\r\n\r\n"));
        assert!(keep(b"GET / HTTP/1.0\r\nConnection: Keep-Alive\r\n\r\n"));
        assert!(!keep(
            b"GET / HTTP/1.1\r\nConnection: keep-alive, close\r\n\r\n"
        ));
    }
}
