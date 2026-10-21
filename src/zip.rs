//! Streaming ZIP writer used to download a whole folder.
//!
//! Entries are stored (no compression) and emitted through `Read`, so the body
//! length is known up front and the archive never has to be buffered on disk or
//! in memory. Sizes come from the directory scan; CRCs are written afterwards
//! in a data descriptor, which is why every local header sets bit 3.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Refuse archives whose bookkeeping would need ZIP64 (fields are 32 bit).
const MAX_TOTAL: u64 = 3_600_000_000;
const MAX_ENTRIES: usize = 65_000;
const COPY_BUF: usize = 64 * 1024;

const LOCAL_SIG: u32 = 0x0403_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const END_SIG: u32 = 0x0605_4b50;
const DESC_SIG: u32 = 0x0807_4b50;
/// Bit 3 defers CRC and sizes to a trailing descriptor, bit 11 marks UTF-8 names.
const FLAGS: u16 = 0x0808;
/// 20 means "PKZIP 2.0", enough for `store`.
const VERSION: u16 = 20;

#[derive(Debug)]
pub enum ZipError {
    TooLarge,
    TooManyEntries,
    Io(io::Error),
}

impl From<io::Error> for ZipError {
    fn from(e: io::Error) -> Self {
        ZipError::Io(e)
    }
}

struct Entry {
    /// Archive path, always `/` separated; directories keep a trailing slash.
    name: String,
    path: PathBuf,
    is_dir: bool,
    size: u64,
    dos_time: u16,
    dos_date: u16,
    crc: u32,
    offset: u32,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Local,
    Data,
    Descriptor,
    Central,
    Done,
}

pub struct ZipStream {
    entries: Vec<Entry>,
    idx: usize,
    phase: Phase,
    buf: Vec<u8>,
    pos: usize,
    file: Option<File>,
    remaining: u64,
    crc: u32,
    central_start: u64,
    produced: u64,
    len: u64,
}

impl ZipStream {
    /// Walks `dir` and prepares an archive that contains `dir` itself.
    pub fn new(dir: &Path) -> Result<ZipStream, ZipError> {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "download".into());
        let mut entries = Vec::new();
        walk(dir, &name, &mut entries)?;

        let mut local_len = 0u64;
        let mut central_len = 0u64;
        for e in &entries {
            local_len += (30 + e.name.len()) as u64 + e.size + 16;
            central_len += (46 + e.name.len()) as u64;
        }
        let len = local_len + central_len + 22;
        if len > MAX_TOTAL {
            return Err(ZipError::TooLarge);
        }
        Ok(ZipStream {
            entries,
            idx: 0,
            phase: Phase::Local,
            buf: Vec::new(),
            pos: 0,
            file: None,
            remaining: 0,
            crc: 0,
            central_start: local_len,
            produced: 0,
            len,
        })
    }

    /// Exact number of bytes this archive will produce.
    pub fn len(&self) -> u64 {
        self.len
    }

    fn push_u16(buf: &mut Vec<u8>, v: u16) {
        buf.extend_from_slice(&v.to_le_bytes());
    }

    fn push_u32(buf: &mut Vec<u8>, v: u32) {
        buf.extend_from_slice(&v.to_le_bytes());
    }

    fn fill(&mut self, bytes: Vec<u8>) {
        self.produced += bytes.len() as u64;
        self.buf = bytes;
        self.pos = 0;
    }

    /// Produces the next piece of output into `self.buf`.
    fn pump(&mut self) -> io::Result<bool> {
        match self.phase {
            Phase::Local => {
                if self.idx >= self.entries.len() {
                    self.idx = 0;
                    self.phase = Phase::Central;
                    return self.pump();
                }
                let e = &self.entries[self.idx];
                let mut buf = Vec::with_capacity(30 + e.name.len());
                Self::push_u32(&mut buf, LOCAL_SIG);
                Self::push_u16(&mut buf, VERSION);
                Self::push_u16(&mut buf, FLAGS);
                Self::push_u16(&mut buf, 0); // stored
                Self::push_u16(&mut buf, e.dos_time);
                Self::push_u16(&mut buf, e.dos_date);
                Self::push_u32(&mut buf, 0); // crc, deferred
                Self::push_u32(&mut buf, 0); // compressed size, deferred
                Self::push_u32(&mut buf, 0); // uncompressed size, deferred
                Self::push_u16(&mut buf, e.name.len() as u16);
                Self::push_u16(&mut buf, 0); // extra field length
                buf.extend_from_slice(e.name.as_bytes());
                let offset = self.produced as u32;
                self.entries[self.idx].offset = offset;
                self.fill(buf);

                self.crc = 0;
                self.remaining = self.entries[self.idx].size;
                if self.entries[self.idx].is_dir {
                    self.phase = Phase::Descriptor;
                } else {
                    self.file = File::open(&self.entries[self.idx].path).ok();
                    self.phase = Phase::Data;
                }
                Ok(true)
            }
            Phase::Data => {
                if self.remaining == 0 {
                    self.phase = Phase::Descriptor;
                    return self.pump();
                }
                let want = self.remaining.min(COPY_BUF as u64) as usize;
                let mut chunk = vec![0u8; want];
                let mut got = 0usize;
                if let Some(file) = self.file.as_mut() {
                    while got < want {
                        match file.read(&mut chunk[got..]) {
                            Ok(0) => break,
                            Ok(n) => got += n,
                            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                            Err(_) => break,
                        }
                    }
                }
                // A file that shrank or vanished since the scan still has to
                // occupy its declared length, otherwise the stream ends early.
                for byte in chunk[got..].iter_mut() {
                    *byte = 0;
                }
                self.crc = crc32(self.crc, &chunk);
                self.remaining -= want as u64;
                self.fill(chunk);
                Ok(true)
            }
            Phase::Descriptor => {
                let e = &self.entries[self.idx];
                let mut buf = Vec::with_capacity(16);
                Self::push_u32(&mut buf, DESC_SIG);
                Self::push_u32(&mut buf, self.crc);
                Self::push_u32(&mut buf, e.size as u32);
                Self::push_u32(&mut buf, e.size as u32);
                self.entries[self.idx].crc = self.crc;
                self.file = None;
                self.fill(buf);
                self.idx += 1;
                self.phase = Phase::Local;
                Ok(true)
            }
            Phase::Central => {
                if self.idx < self.entries.len() {
                    let e = &self.entries[self.idx];
                    let mut buf = Vec::with_capacity(46 + e.name.len());
                    Self::push_u32(&mut buf, CENTRAL_SIG);
                    Self::push_u16(&mut buf, VERSION); // made by
                    Self::push_u16(&mut buf, VERSION); // needed
                    Self::push_u16(&mut buf, FLAGS);
                    Self::push_u16(&mut buf, 0);
                    Self::push_u16(&mut buf, e.dos_time);
                    Self::push_u16(&mut buf, e.dos_date);
                    Self::push_u32(&mut buf, e.crc);
                    Self::push_u32(&mut buf, e.size as u32);
                    Self::push_u32(&mut buf, e.size as u32);
                    Self::push_u16(&mut buf, e.name.len() as u16);
                    Self::push_u16(&mut buf, 0); // extra
                    Self::push_u16(&mut buf, 0); // comment
                    Self::push_u16(&mut buf, 0); // disk
                    Self::push_u16(&mut buf, 0); // internal attributes
                    Self::push_u32(&mut buf, if e.is_dir { 0x10 } else { 0 });
                    Self::push_u32(&mut buf, e.offset);
                    buf.extend_from_slice(e.name.as_bytes());
                    self.fill(buf);
                    self.idx += 1;
                    return Ok(true);
                }
                let count = self.entries.len() as u32;
                let start = self.central_start;
                let central_len = self.produced - start;
                let mut buf = Vec::with_capacity(22);
                Self::push_u32(&mut buf, END_SIG);
                Self::push_u16(&mut buf, 0); // this disk
                Self::push_u16(&mut buf, 0); // disk with the central directory
                Self::push_u16(&mut buf, count as u16);
                Self::push_u16(&mut buf, count as u16);
                Self::push_u32(&mut buf, central_len as u32);
                Self::push_u32(&mut buf, start as u32);
                Self::push_u16(&mut buf, 0); // comment length
                self.fill(buf);
                self.phase = Phase::Done;
                Ok(true)
            }
            Phase::Done => Ok(false),
        }
    }
}

impl Read for ZipStream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.pos < self.buf.len() {
                let n = out.len().min(self.buf.len() - self.pos);
                out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
                self.pos += n;
                return Ok(n);
            }
            if !self.pump()? {
                return Ok(0);
            }
        }
    }
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<Entry>) -> Result<(), ZipError> {
    // Symlinks are skipped on purpose: following them could leave the root or
    // loop forever, and a zip cannot represent them anyway.
    let meta = fs::symlink_metadata(dir)?;
    let (dos_time, dos_date) = dos_datetime(&meta);
    out.push(Entry {
        name: format!("{}/", prefix),
        path: dir.to_path_buf(),
        offset: 0,
        is_dir: true,
        size: 0,
        dos_time,
        dos_date,
        crc: 0,
    });
    if out.len() > MAX_ENTRIES {
        return Err(ZipError::TooManyEntries);
    }

    let mut children: Vec<_> = fs::read_dir(dir)?.filter_map(|e| e.ok()).collect();
    children.sort_by_key(|e| e.file_name());
    for child in children {
        let path = child.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let arc = format!("{}/{}", prefix, child.file_name().to_string_lossy());
        if meta.is_dir() {
            walk(&path, &arc, out)?;
        } else if meta.is_file() {
            let (dos_time, dos_date) = dos_datetime(&meta);
            out.push(Entry {
                name: arc,
                path,
                offset: 0,
                is_dir: false,
                size: meta.len(),
                dos_time,
                dos_date,
                crc: 0,
            });
            if out.len() > MAX_ENTRIES {
                return Err(ZipError::TooManyEntries);
            }
        }
    }
    Ok(())
}

/// MS-DOS timestamp (UTC), the only date format a ZIP entry carries.
fn dos_datetime(meta: &fs::Metadata) -> (u16, u16) {
    let secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = crate::util::civil_from_days(days);
    let year = year.clamp(1980, 2107) as u16;
    let hour = (rem / 3600) as u16;
    let minute = ((rem % 3600) / 60) as u16;
    let second = (rem % 60) as u16;
    (
        (hour << 11) | (minute << 5) | (second / 2),
        ((year - 1980) << 9) | ((month as u16) << 5) | day as u16,
    )
}

/// CRC-32 (IEEE 802.3), as required by the ZIP format.
pub fn crc32(mut crc: u32, data: &[u8]) -> u32 {
    crc = !crc;
    for byte in data {
        crc = CRC_TABLE[((crc ^ *byte as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

const CRC_TABLE: [u32; 256] = build_crc_table();

const fn build_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_known_vector() {
        assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(0, b""), 0);
        // Incremental updates must equal one shot.
        let a = crc32(0, b"1234");
        assert_eq!(crc32(a, b"56789"), 0xCBF4_3926);
    }

    #[test]
    fn streams_a_folder_with_declared_length() {
        let root = std::env::temp_dir().join(format!("fern-ziptest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), b"hello").unwrap();
        fs::write(root.join("sub").join("b.bin"), vec![7u8; 4096]).unwrap();

        let mut zip = ZipStream::new(&root).unwrap();
        let declared = zip.len();
        let mut bytes = Vec::new();
        zip.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.len() as u64, declared, "declared length must match");
        assert_eq!(&bytes[..4], &[0x50, 0x4b, 0x03, 0x04]);
        assert_eq!(
            &bytes[bytes.len() - 22..bytes.len() - 18],
            &[0x50, 0x4b, 0x05, 0x06]
        );
        let text = String::from_utf8_lossy(&bytes).to_string();
        assert!(text.contains("a.txt"));
        assert!(text.contains("sub/b.bin"));
        let _ = fs::remove_dir_all(&root);
    }
}
