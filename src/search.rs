//! Recursive file search.
//!
//! There is no index: a search walks the directory tree on demand. A share can
//! sit on a slow disk or a network mount, so the walk is bounded in four ways —
//! entries examined, depth, results and wall-clock time — and it never fails as
//! a whole because one directory is unreadable. Every bound sets `truncated`
//! instead, which the UI reports, so a partial answer is never mistaken for a
//! complete one.
//!
//! Traversal is iterative on purpose: the server runs one thread per connection
//! with a 128 KiB stack, and a recursive walk would overflow it on a deep tree.

use crate::fsapi;
use crate::trash;
use crate::util::is_hidden_name;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Results returned per request unless the caller asks for more.
pub const DEFAULT_LIMIT: usize = 200;
/// Hard cap on returned results; the UI can page with `offset`.
pub const MAX_LIMIT: usize = 500;
/// Entries examined before the walk gives up. Ten times the usual "one folder
/// per file" layout, so a normal share is fully covered.
pub const MAX_SCAN: usize = 200_000;
/// Depth guard against trees that are deeper than any real backup.
pub const MAX_DEPTH: usize = 64;
/// Wall-clock budget for one request.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// Concurrent walks allowed in the process; a search is far heavier than a
/// listing, so two at a time keep the disk responsive for everyone else.
pub const MAX_CONCURRENT: usize = 2;
/// A timeout check costs a syscall-free `Instant::now()`, but doing it per
/// entry would still show up on small files.
const CLOCK_EVERY: usize = 256;

#[derive(Debug, Clone)]
pub struct Hit {
    pub name: String,
    /// Root relative path, `/a/b/c`.
    pub path: String,
    /// Root relative directory that holds the entry.
    pub parent: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: u64,
    pub hidden: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Treat `*` and `?` in the query as wildcards instead of literal text.
    pub glob: bool,
    /// Include dot files and the server's own temporary names.
    pub show_hidden: bool,
    pub limit: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            glob: false,
            show_hidden: false,
            limit: DEFAULT_LIMIT,
        }
    }
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub hits: Vec<Hit>,
    /// Entries examined, so the UI can say how much of the tree was covered.
    pub scanned: usize,
    pub dirs: usize,
    pub truncated: bool,
}

/// Case-insensitive name matcher. Substring by default; `glob` turns `*` and
/// `?` into wildcards and lets a backslash escape them.
#[derive(Debug, Clone)]
pub enum Matcher {
    Substring(Vec<char>),
    Glob(Vec<char>),
}

impl Matcher {
    pub fn new(query: &str, glob: bool) -> Matcher {
        let folded: Vec<char> = query.to_lowercase().chars().collect();
        if glob {
            Matcher::Glob(folded)
        } else {
            Matcher::Substring(folded)
        }
    }

    pub fn matches(&self, name: &str) -> bool {
        match self {
            Matcher::Substring(needle) => {
                if needle.is_empty() {
                    return true;
                }
                let hay: Vec<char> = name.to_lowercase().chars().collect();
                hay.windows(needle.len()).any(|w| w == needle.as_slice())
            }
            Matcher::Glob(pattern) => {
                glob_match(pattern, &name.to_lowercase().chars().collect::<Vec<_>>())
            }
        }
    }
}

/// `*` matches any run of characters, `?` exactly one, `\` escapes both.
fn glob_match(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    // Position of the last `*` and the text offset it is currently trying.
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while t < text.len() {
        if p < pattern.len() {
            let token = pattern[p];
            if token == '\\' && p + 1 < pattern.len() {
                if pattern[p + 1] == text[t] {
                    p += 2;
                    t += 1;
                    continue;
                }
            } else if token == '?' || token == text[t] {
                p += 1;
                t += 1;
                continue;
            } else if token == '*' {
                star = Some(p);
                mark = t;
                p += 1;
                continue;
            }
        }
        match star {
            // Backtrack: let the star swallow one more character.
            Some(at) => {
                p = at + 1;
                mark += 1;
                t = mark;
            }
            None => return false,
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// Walk `start` (inclusive of its children, not the directory itself) and
/// collect names that match `query`.
pub fn run(root: &Path, start: &Path, query: &str, opts: Options) -> Outcome {
    let matcher = Matcher::new(query, opts.glob);
    if query.trim().is_empty() {
        return Outcome::default();
    }
    let limit = opts.limit.clamp(1, MAX_LIMIT);
    let started = Instant::now();
    let mut out = Outcome::default();
    let mut stack: Vec<(PathBuf, usize)> = vec![(start.to_path_buf(), 0)];
    let mut since_clock = 0usize;

    'walk: while let Some((dir, depth)) = stack.pop() {
        let read_dir = match fs::read_dir(&dir) {
            Ok(r) => r,
            // A directory that vanished, or that the service user cannot read,
            // narrows the result instead of failing the request.
            Err(_) => continue,
        };
        out.dirs += 1;
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let meta = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let file_type = meta.file_type();
            // Links and Windows reparse points (junctions, mounted folders) can
            // point back up the tree; following them would loop or leave the
            // share, so they are skipped exactly like a copy skips them.
            if file_type.is_symlink() || is_reparse_point(&meta) {
                continue;
            }
            let hidden = is_hidden_name(&name) || fsapi::is_internal_name(&name);
            if hidden && !opts.show_hidden {
                continue;
            }
            out.scanned += 1;
            since_clock += 1;
            if since_clock >= CLOCK_EVERY {
                since_clock = 0;
                if started.elapsed() > TIMEOUT {
                    out.truncated = true;
                    break 'walk;
                }
            }
            if out.scanned > MAX_SCAN {
                out.truncated = true;
                break 'walk;
            }

            if matcher.matches(&name) {
                out.hits.push(Hit {
                    name: name.clone(),
                    path: fsapi::relative(root, &path),
                    parent: fsapi::relative(root, &dir),
                    is_dir: file_type.is_dir(),
                    size: if file_type.is_dir() { 0 } else { meta.len() },
                    mtime: mtime_secs(&meta),
                    hidden,
                });
                if out.hits.len() >= limit {
                    out.truncated = true;
                    break 'walk;
                }
            }

            if file_type.is_dir() && depth < MAX_DEPTH {
                // The recycle bin is not part of the browsable tree, and
                // searching it would expose deleted entries.
                if name == fsapi::TRASH_DIR || trash::is_bin_path(root, &path) {
                    continue;
                }
                stack.push((path, depth + 1));
            }
        }
    }
    // Paths sort in one stable order regardless of how the walk recurred.
    out.hits.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn mtime_secs(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(windows)]
fn is_reparse_point(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    // FILE_ATTRIBUTE_REPARSE_POINT covers junctions and volume mount points,
    // which `FileType::is_symlink` does not report.
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_meta: &fs::Metadata) -> bool {
    false
}

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Permission to start one walk. Dropping it always releases the slot, so an
/// early return in the API cannot leak the count.
pub struct Ticket;

pub fn try_enter() -> Option<Ticket> {
    let mut current = ACTIVE.load(Ordering::Relaxed);
    loop {
        if current >= MAX_CONCURRENT {
            return None;
        }
        match ACTIVE.compare_exchange_weak(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Relaxed,
        ) {
            Ok(_) => return Some(Ticket),
            Err(seen) => current = seen,
        }
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::now_secs;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fern-search-{}-{}-{}",
            tag,
            std::process::id(),
            now_secs()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn substring_is_case_insensitive() {
        let m = Matcher::new("Report", false);
        assert!(m.matches("annual-report.pdf"));
        assert!(m.matches("REPORT"));
        assert!(!m.matches("repot"));
        // A literal `*` is not special without glob mode.
        let m = Matcher::new("*.log", false);
        assert!(m.matches("a*.log"));
        assert!(!m.matches("a.log"));
    }

    #[test]
    fn glob_matches_wildcards_and_escapes() {
        let m = Matcher::new("*.log", true);
        assert!(m.matches("server.log"));
        assert!(m.matches(".log"));
        assert!(!m.matches("server.txt"));
        let m = Matcher::new("img_??.png", true);
        assert!(m.matches("IMG_01.PNG"));
        assert!(!m.matches("img_1.png"));
        let m = Matcher::new("a\\*b", true);
        assert!(m.matches("a*b"));
        assert!(!m.matches("axb"));
        // A trailing star must still match a longer name.
        assert!(Matcher::new("doc*", true).matches("documents"));
    }

    #[test]
    fn finds_files_below_the_start_directory() {
        let root = temp_root("walk");
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/b/report.txt"), b"x").unwrap();
        fs::write(root.join("a/note.md"), b"y").unwrap();

        let out = run(&root, &root, "report", Options::default());
        assert_eq!(out.hits.len(), 1);
        assert_eq!(out.hits[0].path, "/a/b/report.txt");
        assert_eq!(out.hits[0].parent, "/a/b");
        assert!(!out.hits[0].is_dir);
        assert!(!out.truncated);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn skips_hidden_names_and_the_recycle_bin() {
        let root = temp_root("skip");
        fs::create_dir_all(root.join(".Trash/abc/data")).unwrap();
        fs::write(root.join(".Trash/abc/data/report.txt"), b"x").unwrap();
        fs::write(root.join(".report"), b"x").unwrap();
        fs::write(root.join("report.txt"), b"x").unwrap();

        let out = run(&root, &root, "report", Options::default());
        assert_eq!(out.hits.len(), 1, "{:?}", out.hits);
        assert_eq!(out.hits[0].path, "/report.txt");

        let shown = Options {
            show_hidden: true,
            ..Options::default()
        };
        let out = run(&root, &root, "report", shown);
        // The hidden file matches now, but the bin stays out of the tree.
        assert_eq!(out.hits.len(), 2, "{:?}", out.hits);
        assert!(out.hits.iter().all(|h| !h.path.contains(".Trash")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn result_limit_truncates_the_walk() {
        let root = temp_root("limit");
        for i in 0..5 {
            fs::write(root.join(format!("hit-{}.txt", i)), b"x").unwrap();
        }
        let out = run(
            &root,
            &root,
            "hit",
            Options {
                limit: 2,
                ..Options::default()
            },
        );
        assert_eq!(out.hits.len(), 2);
        assert!(out.truncated);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_query_matches_nothing() {
        let root = temp_root("empty-q");
        fs::write(root.join("a.txt"), b"x").unwrap();
        let out = run(&root, &root, "   ", Options::default());
        assert!(out.hits.is_empty());
        assert!(!out.truncated);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn concurrent_walks_are_capped() {
        let mut held = Vec::new();
        while let Some(ticket) = try_enter() {
            held.push(ticket);
            if held.len() > MAX_CONCURRENT {
                panic!("more tickets than the cap");
            }
        }
        assert_eq!(held.len(), MAX_CONCURRENT);
        held.pop();
        assert!(try_enter().is_some(), "a released slot must be reusable");
    }
}
