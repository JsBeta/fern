//! Filesystem operations: safe path resolution, listing, trash and uploads.

use crate::util::is_hidden_name;
#[cfg(windows)]
use crate::util::is_reserved_windows_name;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const TRASH_DIR: &str = ".Trash";
pub const PART_SUFFIX: &str = ".fernpart";
/// Temporary sibling used while replacing a file's contents.
pub const EDIT_SUFFIX: &str = ".fernedit";

#[derive(Debug)]
pub enum FsError {
    BadPath,
    NotFound,
    Forbidden,
    Io(io::Error),
}

impl From<io::Error> for FsError {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::NotFound => FsError::NotFound,
            io::ErrorKind::PermissionDenied => FsError::Forbidden,
            _ => FsError::Io(e),
        }
    }
}

impl FsError {
    pub fn status(&self) -> u16 {
        match self {
            FsError::BadPath => 400,
            FsError::NotFound => 404,
            FsError::Forbidden => 403,
            FsError::Io(_) => 500,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            FsError::BadPath => "bad_path",
            FsError::NotFound => "not_found",
            FsError::Forbidden => "forbidden",
            FsError::Io(_) => "io_error",
        }
    }

    pub fn message(&self) -> String {
        match self {
            FsError::BadPath => "invalid path".into(),
            FsError::NotFound => "not found".into(),
            FsError::Forbidden => "permission denied".into(),
            FsError::Io(e) => e.to_string(),
        }
    }
}

/// Joins a decoded URL path onto the shared root, rejecting traversal.
pub fn resolve(root: &Path, rel: &str) -> Result<PathBuf, FsError> {
    let mut out = root.to_path_buf();
    for seg in rel.split(['/', '\\']) {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." || seg.contains('\0') {
            return Err(FsError::BadPath);
        }
        #[cfg(windows)]
        {
            if is_reserved_windows_name(seg) || seg.ends_with('.') || seg.ends_with(' ') {
                return Err(FsError::BadPath);
            }
            if seg.contains(':') {
                return Err(FsError::BadPath);
            }
        }
        out.push(seg);
    }
    Ok(out)
}

/// Rejects paths that escape the root through symlinks or junctions.
pub fn ensure_inside(root: &Path, path: &Path) -> Result<(), FsError> {
    let root_real = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    // Validate through the nearest existing ancestor so a file uploaded into a
    // folder that does not exist yet (a folder upload) is still checked.
    let mut probe = path.to_path_buf();
    loop {
        match fs::canonicalize(&probe) {
            Ok(real) => {
                return if real.starts_with(&root_real) {
                    Ok(())
                } else {
                    Err(FsError::Forbidden)
                };
            }
            Err(_) => match probe.parent() {
                Some(p) if !p.as_os_str().is_empty() => probe = p.to_path_buf(),
                _ => return Err(FsError::NotFound),
            },
        }
    }
}

/// Normalized, root relative path used in API responses (`/` for the root).
pub fn relative(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let mut s = String::from("/");
    let mut first = true;
    for comp in rel.components() {
        if !first {
            s.push('/');
        }
        first = false;
        s.push_str(&comp.as_os_str().to_string_lossy());
    }
    s
}

#[derive(Debug, Clone)]
pub struct EntryInfo {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: u64,
    pub mode: u32,
    pub hidden: bool,
    pub is_link: bool,
}

pub fn list_dir(root: &Path, path: &Path, show_hidden: bool) -> Result<Vec<EntryInfo>, FsError> {
    ensure_inside(root, path)?;
    let mut out = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == TRASH_DIR {
            continue;
        }
        let hidden = is_hidden_name(&name) || is_internal_name(&name);
        if hidden && !show_hidden {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let file_type = meta.file_type();
        let is_link = file_type.is_symlink();
        let mut meta = meta;
        if is_link {
            if let Ok(target_meta) = fs::metadata(entry.path()) {
                meta = target_meta;
            }
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        out.push(EntryInfo {
            name,
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            mtime,
            mode: mode_of(&meta),
            hidden,
            is_link,
        });
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

#[cfg(unix)]
fn mode_of(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    meta.mode() & 0o777
}

#[cfg(not(unix))]
fn mode_of(_meta: &fs::Metadata) -> u32 {
    0
}

pub fn create_dir(root: &Path, path: &Path) -> Result<(), FsError> {
    ensure_inside(root, path)?;
    if path.exists() {
        return Err(FsError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "already exists",
        )));
    }
    fs::create_dir(path)?;
    Ok(())
}

/// Creates an empty file, refusing to overwrite anything already there.
pub fn create_file(root: &Path, path: &Path) -> Result<(), FsError> {
    ensure_inside(root, path)?;
    if path.exists() {
        return Err(FsError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "already exists",
        )));
    }
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    Ok(())
}

pub fn rename(root: &Path, from: &Path, to: &Path) -> Result<(), FsError> {
    ensure_inside(root, from)?;
    ensure_inside(root, to)?;
    if !from.exists() {
        return Err(FsError::NotFound);
    }
    if to.exists() {
        return Err(FsError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "target already exists",
        )));
    }
    if let Some(parent) = to.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::rename(from, to)?;
    Ok(())
}

pub fn remove_entry(path: &Path) -> Result<(), FsError> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// Recursively copies `from` to `to`, which must not exist yet. Symbolic links
/// are skipped so a copy can neither escape the shared root nor loop forever.
pub fn copy_entry(root: &Path, from: &Path, to: &Path) -> Result<(), FsError> {
    ensure_inside(root, from)?;
    ensure_inside(root, to)?;
    let meta = match fs::symlink_metadata(from) {
        Ok(m) => m,
        Err(ref e) if e.kind() == io::ErrorKind::NotFound => return Err(FsError::NotFound),
        Err(e) => return Err(e.into()),
    };
    if to.exists() {
        return Err(FsError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "target already exists",
        )));
    }
    if meta.is_dir() && to.starts_with(from) {
        return Err(FsError::BadPath);
    }
    if let Some(parent) = to.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }
    copy_tree(from, to, &meta)
}

fn copy_tree(from: &Path, to: &Path, meta: &fs::Metadata) -> Result<(), FsError> {
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        fs::create_dir(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            let child = fs::symlink_metadata(entry.path())?;
            copy_tree(&entry.path(), &to.join(entry.file_name()), &child)?;
        }
        let _ = fs::set_permissions(to, meta.permissions());
        Ok(())
    } else {
        fs::copy(from, to)?;
        Ok(())
    }
}

pub struct UploadWriter {
    file: File,
    pub written: u64,
}

impl UploadWriter {
    /// Opens `<target>.fernpart`, seeking to `offset`. Bytes already kept from
    /// an earlier attempt survive unless `restart` explicitly discards them.
    pub fn open(final_path: &Path, offset: u64, restart: bool) -> Result<UploadWriter, FsError> {
        let temp = temp_path(final_path);
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&temp)?;
        if restart {
            file.set_len(0)?;
        } else if file.metadata().map(|m| m.len()).unwrap_or(0) < offset {
            return Err(FsError::BadPath);
        }
        file.seek(SeekFrom::Start(offset))?;
        Ok(UploadWriter { file, written: 0 })
    }

    pub fn write_chunk(&mut self, data: &[u8]) -> Result<(), FsError> {
        self.file.write_all(data)?;
        self.written += data.len() as u64;
        Ok(())
    }

    pub fn finish(self, final_path: &Path) -> Result<(), FsError> {
        let UploadWriter { file, .. } = self;
        file.sync_all()?;
        drop(file);
        let temp = temp_path(final_path);
        if final_path.exists() {
            remove_entry(final_path)?;
        }
        fs::rename(&temp, final_path)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn abort(self, final_path: &Path) {
        let temp = temp_path(final_path);
        drop(self.file);
        let _ = fs::remove_file(temp);
    }
}

/// Drops the leftover `<target>.fernpart` of an abandoned upload.
pub fn discard_partial(final_path: &Path) -> Result<(), FsError> {
    match fs::remove_file(temp_path(final_path)) {
        Ok(()) => Ok(()),
        Err(ref e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Names the server manages itself; they are hidden from listings.
pub fn is_internal_name(name: &str) -> bool {
    name.ends_with(PART_SUFFIX) || name.ends_with(EDIT_SUFFIX)
}

/// Replaces a file's contents through a temporary sibling, keeping the
/// original permission bits (a rename would otherwise reset them on Unix).
pub fn replace_contents(path: &Path, data: &[u8]) -> Result<(), FsError> {
    let temp = edit_temp_path(path);
    let original_mode = fs::metadata(path).ok().map(|m| m.permissions());
    let written = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temp)?;
        file.write_all(data)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(e.into());
    }
    if let Some(perm) = original_mode {
        let _ = fs::set_permissions(&temp, perm);
    }
    if path.exists() {
        if let Err(e) = fs::remove_file(path) {
            let _ = fs::remove_file(&temp);
            return Err(e.into());
        }
    }
    if let Err(e) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(e.into());
    }
    Ok(())
}

fn edit_temp_path(final_path: &Path) -> PathBuf {
    let mut name = final_path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(EDIT_SUFFIX);
    final_path.with_file_name(name)
}

pub fn temp_path(final_path: &Path) -> PathBuf {
    let mut name = final_path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(PART_SUFFIX);
    final_path.with_file_name(name)
}

pub fn existing_partial(final_path: &Path) -> u64 {
    fs::metadata(temp_path(final_path))
        .map(|m| m.len())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::now_secs;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fern-test-{}-{}-{}",
            tag,
            std::process::id(),
            now_secs()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rejects_traversal() {
        let root = PathBuf::from("/srv/web");
        assert!(resolve(&root, "/a/b").is_ok());
        assert!(matches!(resolve(&root, "/../etc"), Err(FsError::BadPath)));
        assert!(matches!(resolve(&root, "/a/../../b"), Err(FsError::BadPath)));
        assert!(resolve(&root, "/a\0b").is_err());
    }

    #[test]
    fn lists_and_hides() {
        let root = temp_root("list");
        fs::write(root.join("a.txt"), b"hello").unwrap();
        fs::write(root.join(".hidden"), b"x").unwrap();
        fs::create_dir(root.join("dir")).unwrap();
        let visible = list_dir(&root, &root, false).unwrap();
        assert_eq!(visible.len(), 2);
        assert!(visible[0].is_dir);
        let all = list_dir(&root, &root, true).unwrap();
        assert_eq!(all.len(), 3);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn upload_resumes_from_offset() {
        let root = temp_root("upload");
        let target = root.join("big.bin");
        let mut w = UploadWriter::open(&target, 0, false).unwrap();
        w.write_chunk(b"hello ").unwrap();
        drop(w);
        let mut w = UploadWriter::open(&target, 6, false).unwrap();
        w.write_chunk(b"world").unwrap();
        w.finish(&target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"hello world");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reopening_at_zero_keeps_data_unless_restarting() {
        let root = temp_root("upload-keep");
        let target = root.join("keep.bin");
        let mut w = UploadWriter::open(&target, 0, false).unwrap();
        w.write_chunk(b"keep me").unwrap();
        drop(w);

        // A plain reopen at zero must not truncate: the caller validates the
        // offset against `existing_partial` and only restarts on request.
        let mut w = UploadWriter::open(&target, 0, false).unwrap();
        w.write_chunk(b"K").unwrap();
        drop(w);
        assert_eq!(existing_partial(&target), 7);

        let mut w = UploadWriter::open(&target, 0, true).unwrap();
        w.write_chunk(b"fresh").unwrap();
        w.finish(&target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"fresh");
        assert!(is_internal_name("a.fernpart"));
        assert!(!is_internal_name("a.txt"));
        let _ = fs::remove_dir_all(&root);
    }
}
