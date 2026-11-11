//! Recycle bin: every deleted entry becomes a self contained folder
//! `<bin>/<id>/` holding `meta.json` and `data/<original name>`, where `<bin>`
//! is the `.Trash` directory of the volume the entry lived on.
//!
//! A share root can span several partitions — sharing `/mnt` while `/mnt/hdd1`
//! and `/mnt/hdd2` are mounted on top is the common case — and a rename cannot
//! cross volumes, so every partition keeps its own bin. Ids stay unique across
//! bins (the ones outside the share root are prefixed with a location tag), so
//! an entry can still be addressed by a plain id.
//!
//! Entries written by older versions (`.Trash/.<base36>.<name>` flat files)
//! are still listed and restorable; they are recognised by the leading dot and
//! are never rewritten, so an upgrade cannot lose data.

use crate::fsapi::{self, FsError, TRASH_DIR};
use crate::json::{self, Json};
use crate::util::{base36, now_secs};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const META_FILE: &str = "meta.json";
const DATA_DIR: &str = "data";
/// Root relative list of the volumes seen so far, kept in the root bin so a
/// plain directory walk is not needed to find the satellite bins again.
const BINS_FILE: &str = "bins.json";
/// Separates the volume tag from the timestamp inside a satellite bin id.
const ID_TAG_SEP: char = '~';
/// Half-built entries while a delete is in flight.
const TMP_PREFIX: &str = ".tmp-";
const TMP_MAX_AGE: u64 = 3600;
/// Directories larger than this are measured only partially so deleting a huge
/// tree never stalls the request.
const MAX_MEASURED: u64 = 50_000;
/// Plausibility floor for a legacy timestamp parsed out of a file name.
const MIN_PLAUSIBLE_STAMP: u64 = 1_000_000_000;

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub name: String,
    /// Root relative path the entry had before it was deleted.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub files: u64,
    pub partial: bool,
    pub deleted_at: u64,
    pub deleted_by: String,
    pub legacy: bool,
}

impl Entry {
    pub fn to_json(&self) -> Json {
        json::obj(vec![
            ("id", Json::Str(self.id.clone())),
            ("name", Json::Str(self.name.clone())),
            ("path", Json::Str(self.path.clone())),
            ("isDir", Json::Bool(self.is_dir)),
            ("size", Json::Num(self.size as i64)),
            ("files", Json::Num(self.files as i64)),
            ("partial", Json::Bool(self.partial)),
            ("deletedAt", Json::Num(self.deleted_at as i64)),
            ("deletedBy", Json::Str(self.deleted_by.clone())),
            ("legacy", Json::Bool(self.legacy)),
        ])
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub enabled: bool,
    pub max_days: u64,
    pub max_bytes: u64,
    pub keep_same: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            enabled: true,
            max_days: 30,
            max_bytes: 1024 * 1024 * 1024,
            keep_same: 10,
        }
    }
}

#[derive(Debug)]
pub struct RestoreError {
    pub code: &'static str,
    pub path: String,
    pub message: String,
}

impl RestoreError {
    fn new(code: &'static str, path: &str, message: &str) -> RestoreError {
        RestoreError {
            code,
            path: path.to_string(),
            message: message.to_string(),
        }
    }
}

/// The bin inside the share root. Volumes mounted below the root keep their
/// own bin; use [`bin_for`] to find the one that serves a given path.
pub fn dir(root: &Path) -> PathBuf {
    root.join(TRASH_DIR)
}

/// True for any path inside a bin, whether it belongs to the share root or to
/// a mounted volume. The file API must never expose these directories.
pub fn is_bin_path(root: &Path, path: &Path) -> bool {
    if path.starts_with(dir(root)) {
        return true;
    }
    match path.strip_prefix(root) {
        Ok(rel) => rel
            .components()
            .any(|c| c.as_os_str() == std::ffi::OsStr::new(TRASH_DIR)),
        Err(_) => false,
    }
}

/// One bin: the directory plus the tag that qualifies the ids of entries it
/// holds. The root bin keeps bare ids so links from older versions still work.
struct Bin {
    path: PathBuf,
    tag: Option<String>,
}

impl Bin {
    fn id_of(&self, local: &str) -> String {
        match &self.tag {
            Some(tag) => format!("{}{}{}", tag, ID_TAG_SEP, local),
            None => local.to_string(),
        }
    }
}

/// Bin that serves `path`: the volume's own `.Trash` when that volume is
/// mounted inside the share, the root bin otherwise. Keeping the bin on the
/// volume of the entry is what lets the move stay a rename.
pub fn bin_for(root: &Path, path: &Path) -> PathBuf {
    match volume_root(root, path) {
        Some(volume) if volume != root => volume.join(TRASH_DIR),
        _ => dir(root),
    }
}

fn anchor(path: &Path) -> Option<PathBuf> {
    let meta = fs::symlink_metadata(path).ok()?;
    if meta.is_dir() {
        Some(path.to_path_buf())
    } else {
        path.parent().map(|p| p.to_path_buf())
    }
}

#[cfg(unix)]
fn volume_root(root: &Path, path: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;

    // Host boundaries can only be seen by comparing device numbers, so climb
    // from the entry towards the share root and stop where the device changes.
    let start = anchor(path)?;
    let dev = fs::metadata(&start).ok()?.dev();
    let mut volume = start;
    while let Some(parent) = volume.parent() {
        if !parent.starts_with(root) {
            break;
        }
        match fs::metadata(parent) {
            Ok(meta) if meta.dev() == dev => volume = parent.to_path_buf(),
            _ => break,
        }
    }
    Some(volume)
}

#[cfg(windows)]
fn volume_root(root: &Path, path: &Path) -> Option<PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    #[link(name = "kernel32")]
    extern "system" {
        fn GetVolumePathNameW(path: *const u16, volume: *mut u16, len: u32) -> i32;
    }

    // Also reports folder mount points, which is exactly the partition bound.
    let start = anchor(path)?;
    let wide: Vec<u16> = start
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut buf = vec![0u16; 1024];
    let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if ok == 0 {
        return None;
    }
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    let volume = PathBuf::from(std::ffi::OsString::from_wide(&buf[..end]));
    if volume.starts_with(root) {
        Some(volume)
    } else {
        None
    }
}

#[cfg(not(any(unix, windows)))]
fn volume_root(_root: &Path, _path: &Path) -> Option<PathBuf> {
    None
}

/// Every bin that may hold entries, the share root one first.
fn bins(root: &Path) -> Vec<Bin> {
    let base = dir(root);
    let mut out = vec![Bin {
        path: base.clone(),
        tag: None,
    }];
    // A single directory level finds the usual `<root>/<disk>/.Trash` layout;
    // the registry remembers volumes that sit deeper or that are new since the
    // last scan.
    for volume in scan_bins(root) {
        push_bin(root, &mut out, &volume);
    }
    for rel in read_bins_file(&base) {
        if let Ok(volume) = fsapi::resolve(root, &rel) {
            push_bin(root, &mut out, &volume);
        }
    }
    out
}

fn push_bin(root: &Path, out: &mut Vec<Bin>, volume: &Path) {
    let path = volume.join(TRASH_DIR);
    if !path.is_dir() || out.iter().any(|b| b.path == path) {
        return;
    }
    let tag = Some(encode_tag(&fsapi::relative(root, volume)));
    out.push(Bin { path, tag });
}

fn scan_bins(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let read_dir = match fs::read_dir(root) {
        Ok(r) => r,
        Err(_) => return out,
    };
    for item in read_dir.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == TRASH_DIR {
            continue;
        }
        let volume = item.path();
        if volume.is_dir() && volume.join(TRASH_DIR).is_dir() {
            out.push(volume);
        }
    }
    out
}

fn read_bins_file(base: &Path) -> Vec<String> {
    let text = match fs::read_to_string(base.join(BINS_FILE)) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let doc = match json::parse(&text) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    doc.get("volumes")
        .and_then(|v| v.as_arr())
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Records a mounted volume so its bin is listed without walking the tree.
fn register_bin(root: &Path, rel: &str) {
    let base = dir(root);
    let mut volumes = read_bins_file(&base);
    if volumes.iter().any(|v| v == rel) {
        return;
    }
    volumes.push(rel.to_string());
    let body = json::obj(vec![(
        "volumes",
        Json::Arr(volumes.into_iter().map(Json::Str).collect()),
    )])
    .dump();
    let _ = fs::create_dir_all(&base);
    let _ = fs::write(base.join(BINS_FILE), body);
}

/// Escapes a root relative volume path into an id prefix.
fn encode_tag(rel: &str) -> String {
    let mut out = String::new();
    for c in rel.trim_start_matches('/').chars() {
        match c {
            '~' => out.push_str("~~"),
            '/' => out.push_str("~1"),
            _ => out.push(c),
        }
    }
    out
}

fn decode_tag(tag: &str) -> Option<String> {
    if tag.is_empty() {
        return None;
    }
    let mut rel = String::from("/");
    let mut chars = tag.chars();
    while let Some(c) = chars.next() {
        if c == ID_TAG_SEP {
            match chars.next()? {
                '~' => rel.push('~'),
                '1' => rel.push('/'),
                _ => return None,
            }
        } else {
            rel.push(c);
        }
    }
    Some(rel)
}

/// Resolves an entry id to its bin and the name of the entry folder inside it.
fn entry_path(root: &Path, id: &str) -> Option<(PathBuf, String)> {
    if !valid_id(id) {
        return None;
    }
    // Legacy entries live in the root bin and may contain the separator, so
    // they are recognised before the tag is looked for.
    if id.starts_with('.') {
        return Some((dir(root), id.to_string()));
    }
    // Either side of the separator can contain the character (a volume named
    // `hdd~1` escapes to `hdd~~1`, a legacy file name may hold a `~`), so try
    // every candidate from the right and keep the first one that decodes to a
    // volume inside the share.
    for (at, _) in id.char_indices().rev() {
        if !id[at..].starts_with(ID_TAG_SEP) {
            continue;
        }
        let local = &id[at + 1..];
        if local.is_empty() {
            continue;
        }
        let rel = match decode_tag(&id[..at]) {
            Some(rel) => rel,
            None => continue,
        };
        let volume = match fsapi::resolve(root, &rel) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if volume == root || !volume.starts_with(root) || !volume.is_dir() {
            continue;
        }
        return Some((volume.join(TRASH_DIR), local.to_string()));
    }
    if id.contains(ID_TAG_SEP) {
        return None;
    }
    Some((dir(root), id.to_string()))
}

#[cfg(unix)]
fn is_cross_device(e: &io::Error) -> bool {
    e.raw_os_error() == Some(18) // EXDEV
}

#[cfg(windows)]
fn is_cross_device(e: &io::Error) -> bool {
    e.raw_os_error() == Some(17) // ERROR_NOT_SAME_DEVICE
}

#[cfg(not(any(unix, windows)))]
fn is_cross_device(_e: &io::Error) -> bool {
    false
}

/// Moves a file or a tree, falling back to copy and delete when the two ends
/// turn out to sit on different filesystems.
fn move_entry(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => {
            copy_any(from, to)?;
            match fsapi::remove_entry(from) {
                Ok(()) => Ok(()),
                Err(_) => {
                    let _ = fsapi::remove_entry(to);
                    Err(io::Error::new(
                        io::ErrorKind::Other,
                        "could not remove the source after copying",
                    ))
                }
            }
        }
        Err(e) => Err(e),
    }
}

fn copy_any(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if !meta.is_dir() {
        fs::copy(from, to)?;
        return Ok(());
    }
    fs::create_dir_all(to)?;
    for item in fs::read_dir(from)? {
        let item = item?;
        copy_any(&item.path(), &to.join(item.file_name()))?;
    }
    let _ = fs::set_permissions(to, meta.permissions());
    Ok(())
}

/// Moves `path` into the recycle bin and returns the new entry id.
pub fn move_to_trash(root: &Path, path: &Path, by: &str) -> Result<String, FsError> {
    fsapi::ensure_inside(root, path)?;
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return Err(FsError::NotFound),
    };
    let name = match path.file_name() {
        Some(n) => n.to_string_lossy().into_owned(),
        None => return Err(FsError::BadPath),
    };
    if name == TRASH_DIR {
        return Err(FsError::Forbidden);
    }
    let orig = fsapi::relative(root, path);
    if orig == "/" {
        return Err(FsError::Forbidden);
    }
    let bin = bin_for(root, path);
    fs::create_dir_all(&bin)?;
    let tag = match bin.parent() {
        Some(volume) if volume != root && volume.starts_with(root) => {
            let rel = fsapi::relative(root, volume);
            register_bin(root, &rel);
            Some(encode_tag(&rel))
        }
        _ => None,
    };

    let now = now_secs();
    let local = next_id(&bin, now);
    let id = match &tag {
        Some(tag) => format!("{}{}{}", tag, ID_TAG_SEP, local),
        None => local.clone(),
    };
    // Stage the entry under a temporary name so a half written folder is never
    // listed, then publish it with a single rename.
    let staging = bin.join(format!("{}{}", TMP_PREFIX, local));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(staging.join(DATA_DIR))?;
    let payload = staging.join(DATA_DIR).join(&name);
    if let Err(e) = move_entry(path, &payload) {
        let _ = fs::remove_dir_all(&staging);
        return Err(e.into());
    }
    let (files, size, partial) = measure(&payload);
    let entry = Entry {
        id: id.clone(),
        name,
        path: orig,
        is_dir: meta.is_dir(),
        size,
        files,
        partial,
        deleted_at: now,
        deleted_by: by.to_string(),
        legacy: false,
    };
    if let Err(e) = write_meta(&staging, &entry) {
        let _ = move_entry(&payload, path);
        let _ = fs::remove_dir_all(&staging);
        return Err(e.into());
    }
    if let Err(e) = fs::rename(&staging, bin.join(&local)) {
        let _ = move_entry(&payload, path);
        let _ = fs::remove_dir_all(&staging);
        return Err(e.into());
    }
    Ok(id)
}

fn next_id(trash: &Path, stamp: u64) -> String {
    let base = base36(stamp);
    for seq in 0..10_000u32 {
        let candidate = if seq == 0 {
            base.clone()
        } else {
            format!("{}-{}", base, seq)
        };
        if !trash.join(&candidate).exists() {
            return candidate;
        }
    }
    format!("{}-{}", base, now_secs())
}

fn write_meta(dir: &Path, e: &Entry) -> io::Result<()> {
    let body = json::obj(vec![
        ("id", Json::Str(e.id.clone())),
        ("name", Json::Str(e.name.clone())),
        ("origPath", Json::Str(e.path.clone())),
        ("isDir", Json::Bool(e.is_dir)),
        ("size", Json::Num(e.size as i64)),
        ("files", Json::Num(e.files as i64)),
        ("partial", Json::Bool(e.partial)),
        ("deletedAt", Json::Num(e.deleted_at as i64)),
        ("deletedBy", Json::Str(e.deleted_by.clone())),
        ("via", Json::Str("delete".into())),
    ])
    .dump();
    fs::write(dir.join(META_FILE), body)
}

/// All entries, newest first. Unreadable entries are skipped instead of
/// failing the whole listing.
pub fn list(root: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    for bin in bins(root) {
        let mut found = Vec::new();
        let read_dir = match fs::read_dir(&bin.path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for item in read_dir.flatten() {
            let local = item.file_name().to_string_lossy().into_owned();
            if local.starts_with(TMP_PREFIX) {
                continue;
            }
            if local.starts_with('.') {
                if let Some(mut e) = legacy_entry(root, &bin.path, &local) {
                    e.id = bin.id_of(&local);
                    found.push(e);
                }
                continue;
            }
            if item.path().is_dir() {
                if let Some(mut e) = entry_from_dir(root, &item.path(), &local) {
                    e.id = bin.id_of(&local);
                    found.push(e);
                }
            }
        }
        out.extend(found);
    }
    out.sort_by(|a, b| {
        b.deleted_at
            .cmp(&a.deleted_at)
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

fn entry_from_dir(root: &Path, entry_dir: &Path, id: &str) -> Option<Entry> {
    if let Some(e) = read_meta(entry_dir, id) {
        if !e.name.is_empty() {
            return Some(e);
        }
    }
    // Missing or damaged meta.json: fall back to what the payload looks like
    // so the entry stays visible and can still be restored or purged.
    let payload = payload_of(entry_dir, "")?;
    let name = payload.file_name()?.to_string_lossy().into_owned();
    let meta = fs::symlink_metadata(&payload).ok()?;
    let (files, size, partial) = measure(&payload);
    let prefix = entry_dir
        .parent()
        .map(|bin| volume_prefix(root, bin))
        .unwrap_or_default();
    Some(Entry {
        id: id.to_string(),
        path: format!("{}/{}", prefix, name),
        name,
        is_dir: meta.is_dir(),
        size,
        files,
        partial,
        deleted_at: mtime_secs(&meta),
        deleted_by: String::new(),
        legacy: false,
    })
}

fn read_meta(entry_dir: &Path, id: &str) -> Option<Entry> {
    let text = fs::read_to_string(entry_dir.join(META_FILE)).ok()?;
    let doc = json::parse(&text).ok()?;
    let name = doc
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let path = doc
        .get("origPath")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("/{}", name));
    Some(Entry {
        id: id.to_string(),
        name,
        path,
        is_dir: doc.get("isDir").and_then(|v| v.as_bool()).unwrap_or(false),
        size: number(&doc, "size"),
        files: number(&doc, "files"),
        partial: doc
            .get("partial")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        deleted_at: number(&doc, "deletedAt"),
        deleted_by: doc
            .get("deletedBy")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        legacy: false,
    })
}

fn number(doc: &Json, key: &str) -> u64 {
    doc.get(key)
        .and_then(|v| v.as_num())
        .filter(|n| *n >= 0)
        .map(|n| n as u64)
        .unwrap_or(0)
}

/// Root relative prefix of the volume a bin belongs to, empty for the share
/// root bin. Entries whose origin is unknown (legacy files, damaged meta) are
/// restored there rather than into the share root.
fn volume_prefix(root: &Path, bin: &Path) -> String {
    match bin.parent() {
        Some(volume) if volume != root => fsapi::relative(root, volume),
        _ => String::new(),
    }
}

fn legacy_entry(root: &Path, base: &Path, id: &str) -> Option<Entry> {
    let full = base.join(id);
    let meta = fs::symlink_metadata(&full).ok()?;
    let stripped = id.strip_prefix('.')?;
    let (stamp, name) = match stripped.split_once('.') {
        Some((s, n)) => (s, n.to_string()),
        None => ("", stripped.to_string()),
    };
    if name.is_empty() {
        return None;
    }
    let (files, size, partial) = measure(&full);
    Some(Entry {
        id: id.to_string(),
        name: name.clone(),
        path: format!("{}/{}", volume_prefix(root, base), name),
        is_dir: meta.is_dir(),
        size,
        files,
        partial,
        deleted_at: base36_decode(stamp).unwrap_or_else(|| mtime_secs(&meta)),
        deleted_by: String::new(),
        legacy: true,
    })
}

fn base36_decode(text: &str) -> Option<u64> {
    let mut value: u64 = 0;
    for c in text.chars() {
        let digit = c.to_digit(36)? as u64;
        value = value.checked_mul(36)?.checked_add(digit)?;
    }
    if value < MIN_PLAUSIBLE_STAMP {
        None
    } else {
        Some(value)
    }
}

fn mtime_secs(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn measure(path: &Path) -> (u64, u64, bool) {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut partial = false;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let meta = match fs::symlink_metadata(&current) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            if let Ok(read_dir) = fs::read_dir(&current) {
                for item in read_dir.flatten() {
                    stack.push(item.path());
                }
            }
            continue;
        }
        files += 1;
        bytes = bytes.saturating_add(meta.len());
        if files >= MAX_MEASURED {
            partial = true;
            break;
        }
    }
    (files, bytes, partial)
}

fn payload_of(entry_dir: &Path, name: &str) -> Option<PathBuf> {
    let data = entry_dir.join(DATA_DIR);
    if !name.is_empty() && !name.contains('/') && !name.contains('\\') {
        let candidate = data.join(name);
        if fs::symlink_metadata(&candidate).is_ok() {
            return Some(candidate);
        }
    }
    let mut found: Option<PathBuf> = None;
    for item in fs::read_dir(&data).ok()?.flatten() {
        if found.is_some() {
            return None;
        }
        found = Some(item.path());
    }
    found
}

fn locate(root: &Path, id: &str) -> Option<(PathBuf, Entry)> {
    let (base, local) = entry_path(root, id)?;
    if local.starts_with('.') {
        let full = base.join(&local);
        fs::symlink_metadata(&full).ok()?;
        let entry = legacy_entry(root, &base, &local)?;
        return Some((full, entry));
    }
    let entry_dir = base.join(&local);
    if !entry_dir.is_dir() {
        return None;
    }
    let mut entry = entry_from_dir(root, &entry_dir, id)?;
    let payload = payload_of(&entry_dir, &entry.name)?;
    if entry.name.is_empty() {
        entry.name = payload.file_name()?.to_string_lossy().into_owned();
        entry.path = format!("/{}", entry.name);
    }
    Some((payload, entry))
}

fn valid_id(id: &str) -> bool {
    if id.is_empty() || id == "." || id == ".." {
        return false;
    }
    !id.contains(['/', '\\', '\0', ':'])
}

/// Puts an entry back where it came from. Returns the restored path.
pub fn restore(root: &Path, id: &str, overwrite: bool) -> Result<String, RestoreError> {
    let (payload, entry) = match locate(root, id) {
        Some(v) => v,
        None => return Err(RestoreError::new("not_found", "", "entry not found")),
    };
    let target = fsapi::resolve(root, &entry.path)
        .map_err(|e| RestoreError::new("bad_path", &entry.path, &e.message()))?;
    if fsapi::ensure_inside(root, &target).is_err() {
        return Err(RestoreError::new(
            "forbidden",
            &entry.path,
            "target escapes the shared root",
        ));
    }
    if target == root || is_bin_path(root, &target) {
        return Err(RestoreError::new(
            "bad_path",
            &entry.path,
            "refusing to restore into the recycle bin",
        ));
    }
    if fs::symlink_metadata(&target).is_ok() {
        if !overwrite {
            return Err(RestoreError::new(
                "exists",
                &entry.path,
                "target already exists",
            ));
        }
        // Keep the conflicting item instead of destroying it.
        move_to_trash(root, &target, "")
            .map_err(|e| RestoreError::new("io_error", &entry.path, &e.message()))?;
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| RestoreError::new("io_error", &entry.path, &e.to_string()))?;
    }
    move_entry(&payload, &target)
        .map_err(|e| RestoreError::new("io_error", &entry.path, &e.to_string()))?;
    if !entry.legacy {
        if let Some((base, local)) = entry_path(root, &entry.id) {
            let _ = fs::remove_dir_all(base.join(local));
        }
    }
    Ok(entry.path)
}

pub fn purge(root: &Path, id: &str) -> bool {
    let (base, local) = match entry_path(root, id) {
        Some(v) => v,
        None => return false,
    };
    let full = base.join(&local);
    if fs::symlink_metadata(&full).is_err() {
        return false;
    }
    fsapi::remove_entry(&full).is_ok()
}

pub fn empty(root: &Path, older_than_days: Option<u64>) -> u32 {
    let cutoff =
        older_than_days.map(|days| now_secs().saturating_sub(days.saturating_mul(86_400)));
    let mut removed = 0;
    for entry in list(root) {
        if let Some(limit) = cutoff {
            if entry.deleted_at >= limit {
                continue;
            }
        }
        if purge(root, &entry.id) {
            removed += 1;
        }
    }
    removed
}

/// Applies the retention policy. Runs at start up and after every delete, so
/// no background thread is needed.
pub fn cleanup(root: &Path, policy: &Policy) -> u32 {
    if !policy.enabled {
        return 0;
    }
    sweep_staging(root);
    let items = list(root);
    let mut doomed: Vec<String> = Vec::new();
    let now = now_secs();

    if policy.max_days > 0 {
        let cutoff = now.saturating_sub(policy.max_days.saturating_mul(86_400));
        for entry in &items {
            if entry.deleted_at > 0 && entry.deleted_at < cutoff {
                doomed.push(entry.id.clone());
            }
        }
    }
    if policy.keep_same > 0 {
        let mut seen: HashMap<&str, u64> = HashMap::new();
        for entry in &items {
            let count = seen.entry(entry.name.as_str()).or_insert(0);
            *count += 1;
            if *count > policy.keep_same {
                doomed.push(entry.id.clone());
            }
        }
    }
    if policy.max_bytes > 0 {
        // `items` is newest first, so the newest entries win the budget and
        // everything past the cap is dropped oldest first.
        let mut total: u64 = 0;
        for entry in &items {
            total = total.saturating_add(entry.size);
            if total > policy.max_bytes {
                doomed.push(entry.id.clone());
            }
        }
    }

    doomed.sort();
    doomed.dedup();
    let mut removed = 0;
    for id in doomed {
        if purge(root, &id) {
            removed += 1;
        }
    }
    removed
}

fn sweep_staging(root: &Path) {
    let now = now_secs();
    for bin in bins(root) {
        let read_dir = match fs::read_dir(&bin.path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for item in read_dir.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            if !name.starts_with(TMP_PREFIX) {
                continue;
            }
            let old = item
                .metadata()
                .ok()
                .map(|m| mtime_secs(&m))
                .unwrap_or(0);
            if now.saturating_sub(old) > TMP_MAX_AGE {
                let _ = fs::remove_dir_all(item.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fern-trash-{}-{}-{}",
            tag,
            std::process::id(),
            now_secs()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn moves_and_records_origin() {
        let root = temp_root("move");
        fs::create_dir_all(root.join("a/b")).unwrap();
        let file = root.join("a/b/c.txt");
        fs::write(&file, b"hello").unwrap();
        let id = move_to_trash(&root, &file, "admin").unwrap();
        assert!(!file.exists());

        let entry_dir = root.join(TRASH_DIR).join(&id);
        assert!(entry_dir.join(META_FILE).is_file());
        assert!(entry_dir.join(DATA_DIR).join("c.txt").is_file());

        let items = list(&root);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, "/a/b/c.txt");
        assert_eq!(items[0].name, "c.txt");
        assert_eq!(items[0].size, 5);
        assert_eq!(items[0].deleted_by, "admin");
        assert!(!items[0].legacy);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn restores_to_original_path() {
        let root = temp_root("restore");
        fs::create_dir_all(root.join("docs")).unwrap();
        let file = root.join("docs/note.txt");
        fs::write(&file, b"body").unwrap();
        let id = move_to_trash(&root, &file, "admin").unwrap();
        assert_eq!(restore(&root, &id, false).unwrap(), "/docs/note.txt");
        assert_eq!(fs::read(&file).unwrap(), b"body");
        assert!(!root.join(TRASH_DIR).join(&id).exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_conflict_needs_overwrite() {
        let root = temp_root("conflict");
        let file = root.join("dup.txt");
        fs::write(&file, b"old").unwrap();
        let id = move_to_trash(&root, &file, "admin").unwrap();
        fs::write(&file, b"new").unwrap();

        let err = restore(&root, &id, false).unwrap_err();
        assert_eq!(err.code, "exists");
        assert_eq!(fs::read(&file).unwrap(), b"new");

        restore(&root, &id, true).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"old");
        // The conflicting file was kept in the bin rather than destroyed.
        assert_eq!(list(&root).len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn restores_directories() {
        let root = temp_root("dir");
        fs::create_dir_all(root.join("pkg/sub")).unwrap();
        fs::write(root.join("pkg/sub/x.bin"), vec![7u8; 1000]).unwrap();
        let id = move_to_trash(&root, &root.join("pkg"), "admin").unwrap();
        let entry = &list(&root)[0];
        assert!(entry.is_dir);
        assert_eq!(entry.files, 1);
        assert_eq!(entry.size, 1000);

        restore(&root, &id, false).unwrap();
        assert_eq!(fs::read(root.join("pkg/sub/x.bin")).unwrap().len(), 1000);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn purges_and_empties() {
        let root = temp_root("purge");
        for name in ["a.txt", "b.txt"] {
            fs::write(root.join(name), b"x").unwrap();
            move_to_trash(&root, &root.join(name), "admin").unwrap();
        }
        assert_eq!(list(&root).len(), 2);
        assert_eq!(empty(&root, None), 2);
        assert!(list(&root).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cleanup_enforces_retention() {
        let root = temp_root("cleanup");
        for name in ["a.txt", "a.txt", "b.txt"] {
            let file = root.join(name);
            fs::write(&file, vec![0u8; 600]).unwrap();
            move_to_trash(&root, &file, "admin").unwrap();
        }
        // Three entries, 1200 bytes total, keep at most two same-name copies.
        let policy = Policy {
            enabled: true,
            max_days: 0,
            max_bytes: 0,
            keep_same: 1,
        };
        assert_eq!(cleanup(&root, &policy), 1);

        let policy = Policy {
            enabled: true,
            max_days: 0,
            max_bytes: 1000,
            keep_same: 0,
        };
        assert_eq!(cleanup(&root, &policy), 1);
        assert_eq!(list(&root).len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn lists_and_restores_legacy_entries() {
        let root = temp_root("legacy");
        let trash = root.join(TRASH_DIR);
        fs::create_dir_all(&trash).unwrap();
        let stamp = base36(1_700_000_000);
        fs::write(trash.join(format!(".{}.old.txt", stamp)), b"legacy").unwrap();

        let items = list(&root);
        assert_eq!(items.len(), 1);
        assert!(items[0].legacy);
        assert_eq!(items[0].name, "old.txt");
        assert_eq!(items[0].path, "/old.txt");
        assert_eq!(items[0].deleted_at, 1_700_000_000);

        restore(&root, &items[0].id, false).unwrap();
        assert_eq!(fs::read(root.join("old.txt")).unwrap(), b"legacy");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ignores_half_written_entries() {
        let root = temp_root("staging");
        let trash = root.join(TRASH_DIR);
        fs::create_dir_all(trash.join(".tmp-abc/data")).unwrap();
        assert!(list(&root).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_ids_that_escape_the_bin() {
        let root = temp_root("escape");
        fs::create_dir_all(&root).unwrap();
        assert!(!purge(&root, "../x"));
        assert!(!purge(&root, ".."));
        assert!(restore(&root, "../x", false).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn volume_tags_round_trip() {
        for rel in ["/disk", "/mnt/hdd~1", "/a b/c"] {
            let tag = encode_tag(rel);
            assert!(!tag.contains('/'), "{:?} kept a separator", tag);
            assert_eq!(decode_tag(&tag).as_deref(), Some(rel));
        }
        assert!(decode_tag("").is_none());
        assert!(decode_tag("bad~2").is_none());
    }

    #[test]
    fn root_bin_keeps_bare_ids() {
        let root = temp_root("bare");
        let file = root.join("keep.txt");
        fs::write(&file, b"x").unwrap();
        let id = move_to_trash(&root, &file, "admin").unwrap();
        assert!(!id.contains(ID_TAG_SEP), "root ids must stay bare: {}", id);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn lists_and_restores_entries_from_a_mounted_bin() {
        let root = temp_root("mounted");
        let volume = root.join("hdd1");
        let bin = volume.join(TRASH_DIR);
        let entry_dir = bin.join("abc");
        fs::create_dir_all(entry_dir.join(DATA_DIR)).unwrap();
        fs::write(entry_dir.join(DATA_DIR).join("doc.txt"), b"body").unwrap();
        let entry = Entry {
            id: "abc".into(),
            name: "doc.txt".into(),
            path: "/hdd1/doc.txt".into(),
            is_dir: false,
            size: 4,
            files: 1,
            partial: false,
            deleted_at: now_secs(),
            deleted_by: "admin".into(),
            legacy: false,
        };
        write_meta(&entry_dir, &entry).unwrap();
        assert_eq!(bins(&root).len(), 2);

        let items = list(&root);
        assert_eq!(items.len(), 1);
        let id = items[0].id.clone();
        assert_eq!(id, format!("hdd1{}abc", ID_TAG_SEP));

        assert!(!purge(&root, "abc"), "a satellite entry needs its tag");
        assert_eq!(restore(&root, &id, false).unwrap(), "/hdd1/doc.txt");
        assert_eq!(fs::read(volume.join("doc.txt")).unwrap(), b"body");
        assert!(!entry_dir.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bin_registry_is_remembered() {
        let root = temp_root("registry");
        fs::create_dir_all(root.join("hdd2").join(TRASH_DIR)).unwrap();
        assert_eq!(bins(&root).len(), 2);
        register_bin(&root, "/hdd2");
        assert_eq!(read_bins_file(&dir(&root)), vec!["/hdd2".to_string()]);
        assert_eq!(bins(&root).len(), 2, "the volume must not be added twice");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn detects_a_nested_bin_path() {
        let root = temp_root("binpath");
        assert!(is_bin_path(&root, &root.join(TRASH_DIR).join("x")));
        assert!(is_bin_path(&root, &root.join("hdd1").join(TRASH_DIR)));
        assert!(!is_bin_path(&root, &root.join("hdd1").join("doc.txt")));
        assert!(!is_bin_path(&root, Path::new("/elsewhere/.Trash")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn resolves_a_tag_that_contains_the_separator() {
        let root = temp_root("taggys");
        let bin = root.join("hdd~1").join(TRASH_DIR);
        fs::create_dir_all(bin.join("abc")).unwrap();
        let tagged = format!("hdd~~1{}abc", ID_TAG_SEP);
        let (base, local) = entry_path(&root, &tagged).expect("tagged id must resolve");
        assert_eq!(base, bin);
        assert_eq!(local, "abc");
        assert!(entry_path(&root, "nope~abc").is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn tags_legacy_entries_inside_a_mounted_bin() {
        let root = temp_root("legacybin");
        let bin = root.join("hdd1").join(TRASH_DIR);
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join(".1700000000.old.txt"), b"legacy").unwrap();

        let items = list(&root);
        assert_eq!(items.len(), 1);
        assert!(items[0].legacy);
        let id = items[0].id.clone();
        assert_eq!(id, format!("hdd1{}.1700000000.old.txt", ID_TAG_SEP));

        assert_eq!(restore(&root, &id, false).unwrap(), "/hdd1/old.txt");
        assert_eq!(
            fs::read(root.join("hdd1").join("old.txt")).unwrap(),
            b"legacy"
        );
        assert!(!bin.join(".1700000000.old.txt").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
