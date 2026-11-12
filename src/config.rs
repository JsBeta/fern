//! `fern.conf` parsing plus environment overrides.

use crate::cli::{Action, Options};
use std::path::{Path, PathBuf};

pub const PERM_READ: u32 = 1;
pub const PERM_LIST: u32 = 2;
pub const PERM_UPLOAD: u32 = 4;
pub const PERM_MODIFY: u32 = 8;
pub const PERM_SHOW_HIDDEN: u32 = 16;
pub const PERM_PLAY: u32 = 32;
pub const PERM_DOWNLOAD: u32 = 64;

pub const DEFAULT_PORT: u16 = 4545;
pub const DEFAULT_MAX_CONNS: usize = 128;
pub const DEFAULT_TITLE: &str = "fern";
/// Upper bound for `Fern.Title` and `Fern.Notice`, keeps the top bar usable.
pub const MAX_BRAND_CHARS: usize = 200;

/// `Fern.LogLevel`: how much of the log reaches the console, the daily file
/// and syslog. `Error` shows the least.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

impl Default for LogLevel {
    fn default() -> Self {
        LogLevel::Info
    }
}

impl LogLevel {
    pub fn parse(text: &str) -> Option<LogLevel> {
        match text.trim().to_ascii_lowercase().as_str() {
            "error" | "err" | "0" => Some(LogLevel::Error),
            "warn" | "warning" | "1" => Some(LogLevel::Warn),
            "info" | "2" => Some(LogLevel::Info),
            "debug" | "3" => Some(LogLevel::Debug),
            _ => None,
        }
    }

    /// Canonical spelling used when the configuration is written back.
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
        }
    }

    /// Upper-case tag written in front of every log line.
    pub fn tag(self) -> &'static str {
        match self {
            LogLevel::Error => "ERROR",
            LogLevel::Warn => "WARN",
            LogLevel::Info => "INFO",
            LogLevel::Debug => "DEBUG",
        }
    }

    /// Severity rank: a message is written when its rank is not above the
    /// configured one. Also the index of the level in the tray combo box.
    pub fn rank(self) -> u8 {
        match self {
            LogLevel::Error => 0,
            LogLevel::Warn => 1,
            LogLevel::Info => 2,
            LogLevel::Debug => 3,
        }
    }

    /// Inverse of `rank`, used by the tray combo box and its stored index.
    pub fn from_rank(rank: u8) -> LogLevel {
        match rank {
            0 => LogLevel::Error,
            1 => LogLevel::Warn,
            3 => LogLevel::Debug,
            _ => LogLevel::Info,
        }
    }
}

/// One directive owned by the tray configuration window: the lower-case
/// directive name together with the lines that replace every occurrence of it.
/// An empty line list removes the directive, which restores its default.
pub type DirectiveBlock = (&'static str, Vec<String>);

#[derive(Debug, Clone)]
pub struct User {
    pub name: String,
    pub pass: String,
    pub perms: u32,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    pub listen: Vec<String>,
    pub users: Vec<User>,
    pub guest: u32,
    pub hide: bool,
    pub browser: Option<String>,
    pub open_browser: bool,
    pub log_dir: Option<PathBuf>,
    pub syslog: bool,
    pub fd_limit: Option<u64>,
    pub runas: Option<String>,
    pub chroot: Option<PathBuf>,
    pub max_conns: usize,
    pub thread_stack: usize,
    /// `Fern.Title`: text shown in the browser tab and the top bar brand.
    pub title: String,
    /// `Fern.Notice`: optional banner text shown in the top bar.
    pub notice: String,
    /// `Fern.LogLevel`: verbosity of the console, file and syslog output.
    pub log_level: LogLevel,
    pub mime_files: Vec<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub action: Option<Action>,
    /// `Trash.Enabled`: delete moves entries into `.Trash` instead of removing them.
    pub trash_enabled: bool,
    /// `Trash.Max.Days`: entries older than this are dropped (0 disables).
    pub trash_max_days: u64,
    /// `Trash.Max.Size`: total size cap of `.Trash` in MB (0 disables).
    pub trash_max_mb: u64,
    /// `Trash.Keep.Same`: how many copies of one file name survive.
    pub trash_keep_same: u64,
    /// Whether `Fern.Root` or `-w` chose the share, as opposed to falling back
    /// to the directory holding the executable.
    root_explicit: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            // Placeholder only: `load` replaces it with the directory holding
            // the executable unless `Fern.Root` or `-w` says otherwise.
            root: PathBuf::from("web"),
            // Filled during `load`; the default port only applies when no
            // `Fern.Listen` line and no `-l` option were given.
            listen: Vec::new(),
            users: Vec::new(),
            guest: PERM_READ | PERM_LIST,
            hide: false,
            browser: None,
            open_browser: true,
            log_dir: None,
            syslog: false,
            fd_limit: None,
            runas: None,
            chroot: None,
            max_conns: DEFAULT_MAX_CONNS,
            thread_stack: 128 * 1024,
            title: DEFAULT_TITLE.to_string(),
            notice: String::new(),
            log_level: LogLevel::default(),
            mime_files: Vec::new(),
            config_path: None,
            action: None,
            trash_enabled: true,
            trash_max_days: 30,
            trash_max_mb: 1024,
            trash_keep_same: 10,
            root_explicit: false,
        }
    }
}

impl Config {
    pub fn trash_policy(&self) -> crate::trash::Policy {
        crate::trash::Policy {
            enabled: self.trash_enabled,
            max_days: self.trash_max_days,
            max_bytes: self.trash_max_mb.saturating_mul(1024 * 1024),
            keep_same: self.trash_keep_same,
        }
    }

    /// Parses `text` as a standalone `fern.conf`.
    ///
    /// The tray configuration window uses it to check the merged file before
    /// it replaces the one on disk, so a typo never costs the running
    /// configuration.
    pub fn from_text(text: &str, origin: &Path) -> Result<Config, String> {
        let mut cfg = Config::default();
        apply_conf_text(&mut cfg, text, origin)?;
        cfg.root = absolute(&cfg.root);
        Ok(cfg)
    }

    /// Directives owned by the tray configuration window, rendered from the
    /// values currently in effect. `merge_directives` writes them back.
    pub fn editable_blocks(&self) -> Vec<DirectiveBlock> {
        let mut blocks: Vec<DirectiveBlock> = Vec::new();
        blocks.push((
            "fern.root",
            vec![format!(
                "Fern.Root {}",
                quote_value(&self.root.to_string_lossy())
            )],
        ));
        blocks.push((
            "fern.listen",
            self.listen
                .iter()
                .map(|l| format!("Fern.Listen {}", quote_value(l)))
                .collect(),
        ));
        blocks.push((
            "fern.loglevel",
            vec![format!("Fern.LogLevel {}", self.log_level.as_str())],
        ));
        blocks.push((
            "fern.maxconns",
            vec![format!("Fern.MaxConns {}", self.max_conns)],
        ));
        blocks.push((
            "fern.guest",
            vec![format!("Fern.Guest {}", perms_text(self.guest))],
        ));
        blocks.push((
            "fern.title",
            vec![format!("Fern.Title {}", quote_value(&self.title))],
        ));
        blocks.push((
            "fern.notice",
            if self.notice.is_empty() {
                Vec::new()
            } else {
                vec![format!("Fern.Notice {}", quote_value(&self.notice))]
            },
        ));
        blocks.push((
            "fern.logdir",
            match &self.log_dir {
                Some(dir) => vec![format!(
                    "Fern.LogDir {}",
                    quote_value(&dir.to_string_lossy())
                )],
                None => Vec::new(),
            },
        ));
        blocks.push((
            "trash.enabled",
            vec![format!(
                "Trash.Enabled {}",
                if self.trash_enabled { "1" } else { "0" }
            )],
        ));
        blocks.push((
            "trash.max.days",
            vec![format!("Trash.Max.Days {}", self.trash_max_days)],
        ));
        blocks
    }
}

impl Config {
    pub fn load(opts: &Options) -> Result<Config, String> {
        let mut cfg = Config::default();
        cfg.action = opts.action;

        let path = match &opts.config {
            Some(p) => {
                if !p.exists() {
                    return Err(format!("configuration file not found: {}", p.display()));
                }
                Some(p.clone())
            }
            None => find_default_config(),
        };
        let conf_found = path.is_some();
        if let Some(p) = &path {
            let text = std::fs::read_to_string(p)
                .map_err(|e| format!("cannot read {}: {}", p.display(), e))?;
            apply_conf_text(&mut cfg, &text, p)?;
            cfg.config_path = Some(p.clone());
        }

        if let Some(root) = &opts.root {
            cfg.root = PathBuf::from(root);
            cfg.root_explicit = true;
        }
        if !opts.listen.is_empty() {
            cfg.listen = opts.listen.clone();
        }
        if let Some(g) = opts.guest {
            cfg.guest = g;
        }
        for u in &opts.users {
            cfg.users.push(u.clone());
        }
        if opts.hide {
            cfg.hide = true;
        }
        if !opts.open_browser {
            cfg.open_browser = false;
        }
        if let Some(d) = &opts.log_dir {
            cfg.log_dir = Some(d.clone());
        }
        if let Some(n) = opts.max_conns {
            cfg.max_conns = n.max(1);
        }

        apply_env(&mut cfg);
        // Nothing configured the share: serve the folder holding the
        // executable, so a bare fern.exe can be dropped anywhere and run.
        if !cfg.root_explicit && !conf_found {
            cfg.root = exe_dir();
        }
        push_default_mime_files(&mut cfg);
        cfg.root = absolute(&cfg.root);
        if cfg.listen.is_empty() {
            cfg.listen.push(DEFAULT_PORT.to_string());
        }
        if cfg.max_conns == 0 {
            cfg.max_conns = DEFAULT_MAX_CONNS;
        }
        Ok(cfg)
    }

    pub fn user(&self, name: &str) -> Option<&User> {
        self.users.iter().find(|u| u.name == name)
    }
}

fn push_default_mime_files(cfg: &mut Config) {
    let mut candidates: Vec<PathBuf> = vec![PathBuf::from("fern.mime")];
    if let Some(path) = &cfg.config_path {
        if let Some(dir) = path.parent() {
            candidates.push(dir.join("fern.mime"));
        }
    }
    for candidate in candidates {
        if candidate.is_file() && !cfg.mime_files.contains(&candidate) {
            cfg.mime_files.push(candidate);
        }
    }
}

fn apply_env(cfg: &mut Config) {
    if let Ok(v) = std::env::var("_LOG_DIR") {
        if !v.is_empty() {
            cfg.log_dir = Some(PathBuf::from(v));
        }
    }
    if let Ok(v) = std::env::var("_syslog") {
        cfg.syslog = truthy(&v);
    }
    if let Ok(v) = std::env::var("_FD_LIMIT") {
        if let Ok(n) = v.trim().parse::<u64>() {
            cfg.fd_limit = Some(n);
        }
    }
    if let Ok(v) = std::env::var("_RUNAS") {
        if !v.is_empty() {
            cfg.runas = Some(v);
        }
    }
    if let Ok(v) = std::env::var("_CHROOT_PATH") {
        if !v.is_empty() {
            cfg.chroot = Some(PathBuf::from(v));
        }
    }
    if let Ok(v) = std::env::var("_MAX_CONNS") {
        if let Ok(n) = v.trim().parse::<usize>() {
            cfg.max_conns = n.max(1);
        }
    }
    if let Ok(v) = std::env::var("_TRASH_ENABLED") {
        cfg.trash_enabled = truthy(&v);
    }
    if let Ok(v) = std::env::var("_TRASH_MAX_DAYS") {
        if let Ok(n) = v.trim().parse::<u64>() {
            cfg.trash_max_days = n;
        }
    }
    if let Ok(v) = std::env::var("_TRASH_MAX_SIZE") {
        if let Ok(n) = v.trim().parse::<u64>() {
            cfg.trash_max_mb = n;
        }
    }
    if let Ok(v) = std::env::var("_TRASH_KEEP_SAME") {
        if let Ok(n) = v.trim().parse::<u64>() {
            cfg.trash_keep_same = n;
        }
    }
}

fn truthy(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub fn find_default_config() -> Option<PathBuf> {
    let local = PathBuf::from("fern.conf");
    if local.is_file() {
        return Some(local);
    }
    #[cfg(unix)]
    {
        let etc = PathBuf::from("/etc/fern.conf");
        if etc.is_file() {
            return Some(etc);
        }
    }
    None
}

pub fn parse_perms(text: &str) -> Option<u32> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if t == "0" {
        return Some(0);
    }
    let mut perms = 0u32;
    for c in t.chars() {
        match c {
            'r' | 'R' => perms |= PERM_READ,
            'l' | 'L' => perms |= PERM_LIST,
            'u' | 'U' => perms |= PERM_UPLOAD,
            'm' | 'M' => perms |= PERM_MODIFY,
            's' | 'S' => perms |= PERM_SHOW_HIDDEN,
            't' | 'T' => perms |= PERM_PLAY,
            'd' | 'D' => perms |= PERM_DOWNLOAD,
            _ => return None,
        }
    }
    Some(perms)
}

/// Letters for `perms`, or `"0"` when nothing is granted.
///
/// `perms_to_letters(0)` is the empty string, which reads badly in a
/// configuration file and in the form; the explicit `0` is also what
/// `parse_perms` accepts back.
pub fn perms_text(perms: u32) -> String {
    if perms == 0 {
        "0".to_string()
    } else {
        perms_to_letters(perms)
    }
}

pub fn perms_to_letters(perms: u32) -> String {
    let mut s = String::new();
    if perms & PERM_READ != 0 {
        s.push('R');
    }
    if perms & PERM_LIST != 0 {
        s.push('L');
    }
    if perms & PERM_UPLOAD != 0 {
        s.push('U');
    }
    if perms & PERM_MODIFY != 0 {
        s.push('M');
    }
    if perms & PERM_SHOW_HIDDEN != 0 {
        s.push('S');
    }
    if perms & PERM_PLAY != 0 {
        s.push('T');
    }
    if perms & PERM_DOWNLOAD != 0 {
        s.push('D');
    }
    s
}

fn apply_conf_text(cfg: &mut Config, text: &str, origin: &Path) -> Result<(), String> {
    for (idx, raw) in text.lines().enumerate() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = split_tokens(line);
        if parts.is_empty() {
            continue;
        }
        let key = parts.remove(0).to_ascii_lowercase();
        let err = |msg: &str| format!("{}:{}: {}", origin.display(), idx + 1, msg);
        match key.as_str() {
            "fern.root" => {
                let v = parts.first().ok_or_else(|| err("Fern.Root needs a path"))?;
                cfg.root = PathBuf::from(v);
                cfg.root_explicit = true;
            }
            "fern.listen" => {
                let v = parts
                    .first()
                    .ok_or_else(|| err("Fern.Listen needs an address"))?;
                cfg.listen.push(v.clone());
            }
            "fern.user" => {
                if parts.len() != 3 {
                    return Err(err("Fern.User needs <permissions> <user> <password>"));
                }
                let perms = parse_perms(&parts[0])
                    .ok_or_else(|| err("Fern.User permissions must use letters RLUMSTD"))?;
                cfg.users.push(User {
                    name: parts[1].clone(),
                    pass: parts[2].clone(),
                    perms,
                });
            }
            "fern.guest" => {
                let v = parts.first().ok_or_else(|| err("Fern.Guest needs permissions"))?;
                cfg.guest = parse_perms(v)
                    .ok_or_else(|| err("Fern.Guest permissions must use letters RLUMSTD"))?;
            }
            "fern.title" => {
                cfg.title = brand_text(&parts, "Fern.Title").map_err(|m| err(&m))?;
            }
            "fern.notice" => {
                cfg.notice = brand_text(&parts, "Fern.Notice").map_err(|m| err(&m))?;
            }
            "fern.loglevel" => {
                let v = parts
                    .first()
                    .ok_or_else(|| err("Fern.LogLevel needs a level"))?;
                cfg.log_level = LogLevel::parse(v)
                    .ok_or_else(|| err("Fern.LogLevel must be error, warn, info or debug"))?;
            }
            "fern.hide" => {
                cfg.hide = parts.first().map(|v| truthy(v)).unwrap_or(true);
            }
            "fern.browser" => {
                cfg.browser = parts.first().cloned();
            }
            "fern.maxconns" => {
                if let Some(n) = parts.first().and_then(|v| v.parse::<usize>().ok()) {
                    cfg.max_conns = n.max(1);
                }
            }
            "fern.logdir" => {
                if let Some(v) = parts.first() {
                    cfg.log_dir = Some(PathBuf::from(v));
                }
            }
            "fern.mime" => {
                let v = parts.first().ok_or_else(|| err("Fern.Mime needs a file path"))?;
                cfg.mime_files.push(PathBuf::from(v));
            }
            "trash.enabled" => {
                cfg.trash_enabled = parts.first().map(|v| truthy(v)).unwrap_or(true);
            }
            "trash.max.days" => {
                let v = parts.first().ok_or_else(|| err("Trash.Max.Days needs a number"))?;
                cfg.trash_max_days = v
                    .parse::<u64>()
                    .map_err(|_| err("Trash.Max.Days must be a whole number of days"))?;
            }
            "trash.max.size" => {
                let v = parts.first().ok_or_else(|| err("Trash.Max.Size needs a number"))?;
                cfg.trash_max_mb = v
                    .parse::<u64>()
                    .map_err(|_| err("Trash.Max.Size must be a whole number of megabytes"))?;
            }
            "trash.keep.same" => {
                let v = parts.first().ok_or_else(|| err("Trash.Keep.Same needs a number"))?;
                cfg.trash_keep_same = v
                    .parse::<u64>()
                    .map_err(|_| err("Trash.Keep.Same must be a whole number"))?;
            }
            other => return Err(err(&format!("unknown directive: {}", other))),
        }
    }
    Ok(())
}

/// Re-joins the tokens of a free-form directive so unquoted multi word values
/// keep their spaces, and caps the length so the top bar stays readable.
fn brand_text(parts: &[String], name: &str) -> Result<String, String> {
    let text = parts.join(" ");
    let text = text.trim();
    if text.is_empty() {
        return Err(format!("{} needs a text value", name));
    }
    if text.chars().count() > MAX_BRAND_CHARS {
        return Err(format!(
            "{} is limited to {} characters",
            name, MAX_BRAND_CHARS
        ));
    }
    Ok(text.to_string())
}

/// Rewrites `original` so that every directive named in `blocks` carries the
/// new lines. Lines the window does not own — comments, blank lines and
/// directives it never shows — are preserved verbatim, and a directive with an
/// empty line list disappears so its default applies again.
pub fn merge_directives(original: &str, blocks: &[DirectiveBlock]) -> String {
    // Keep the file's own line endings; hand-edited Windows files are CRLF and
    // rewriting them with bare LF would show up as a whole-file diff.
    let eol = if original.contains("\r\n") { "\r\n" } else { "\n" };
    let mut pending: Vec<Option<&DirectiveBlock>> = blocks.iter().map(Some).collect();
    let mut out: Vec<String> = Vec::new();
    for raw in original.lines() {
        let key = directive_key(raw);
        let owned = key
            .as_ref()
            .and_then(|key| blocks.iter().position(|b| b.0 == key.as_str()));
        match owned {
            Some(i) => {
                // A repeated directive is replaced by the block at its first
                // occurrence; the leftover copies would otherwise survive and
                // duplicate the new value.
                if let Some(block) = pending[i].take() {
                    out.extend(block.1.iter().cloned());
                }
            }
            None => out.push(raw.to_string()),
        }
    }
    for block in pending.into_iter().flatten() {
        if block.1.is_empty() {
            continue;
        }
        if out.last().map(|l| !l.is_empty()).unwrap_or(false) {
            out.push(String::new());
        }
        out.extend(block.1.iter().cloned());
    }
    let mut text = out.join(eol);
    if !text.is_empty() {
        text.push_str(eol);
    }
    text
}

/// Lower-cased directive name of one line, ignoring comments and indentation.
fn directive_key(raw: &str) -> Option<String> {
    let line = strip_comment(raw).trim();
    if line.is_empty() {
        return None;
    }
    line.split_whitespace().next().map(|k| k.to_ascii_lowercase())
}

/// Quotes a value so the tokenizer keeps it in one piece.
pub fn quote_value(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || c == '#' || c == '"');
    if needs_quotes {
        format!("\"{}\"", value.replace('"', "'"))
    } else {
        value.to_string()
    }
}

fn strip_comment(line: &str) -> &str {
    let mut in_quotes = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            '#' if !in_quotes => return &line[..i],
            _ => {}
        }
    }
    line
}

pub fn split_tokens(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in line.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Directory holding the running executable, used as the share root when no
/// configuration file and no `-w`/`Fern.Root` chose one.
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        normalize(p)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        normalize(&cwd.join(p))
    }
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_port_is_4545() {
        assert_eq!(DEFAULT_PORT, 4545);
    }

    #[test]
    fn permission_letters() {
        assert_eq!(parse_perms("RL").unwrap(), 3);
        assert_eq!(parse_perms("RLUMSTD").unwrap(), 127);
        // Lowercase input is still accepted, uppercase is the canonical form.
        assert_eq!(parse_perms("rlumstd").unwrap(), 127);
        assert_eq!(parse_perms("0").unwrap(), 0);
        assert!(parse_perms("x").is_none());
        assert_eq!(perms_to_letters(3), "RL");
        assert_eq!(perms_to_letters(127), "RLUMSTD");
        assert_eq!(perms_text(0), "0");
        assert_eq!(perms_text(3), "RL");
        assert_eq!(parse_perms(&perms_text(0)), Some(0));
    }

    #[test]
    fn default_root_is_exe_dir() {
        let cfg = Config::load(&Options::default()).unwrap();
        assert!(!cfg.root_explicit);
        assert_eq!(cfg.root, absolute(&exe_dir()));
        assert_ne!(cfg.root, absolute(Path::new("web")));
    }

    #[test]
    fn explicit_root_wins() {
        let mut cfg = Config::default();
        apply_conf_text(&mut cfg, "Fern.Root /data/share", Path::new("x")).unwrap();
        assert!(cfg.root_explicit);

        let opts = Options {
            root: Some("data".to_string()),
            ..Default::default()
        };
        let cfg = Config::load(&opts).unwrap();
        assert!(cfg.root_explicit);
        assert_eq!(cfg.root, absolute(Path::new("data")));
    }

    #[test]
    fn parses_conf_text() {
        let text = r#"
# comment
Fern.Root   "/data/my share"
Fern.Listen 4545
Fern.Listen 127.0.0.1:9000
Fern.User RLUM alice secret
Fern.Guest RL
Fern.Hide 1
"#;
        let mut cfg = Config::default();
        apply_conf_text(&mut cfg, text, Path::new("fern.conf")).unwrap();
        assert_eq!(cfg.root, PathBuf::from("/data/my share"));
        assert_eq!(cfg.listen, vec!["4545", "127.0.0.1:9000"]);
        assert_eq!(cfg.users[0].name, "alice");
        assert_eq!(cfg.users[0].perms, 15);
        assert_eq!(cfg.guest, 3);
        assert!(cfg.hide);
    }

    #[test]
    fn rejects_unknown_directive() {
        let mut cfg = Config::default();
        assert!(apply_conf_text(&mut cfg, "Fern.Bogus 1", Path::new("x")).is_err());
    }

    #[test]
    fn branding_defaults_and_overrides() {
        let mut cfg = Config::default();
        assert_eq!(cfg.title, "fern");
        assert_eq!(cfg.notice, "");

        apply_conf_text(
            &mut cfg,
            "Fern.Title \"我的 网盘\"\nFern.Notice 服务器将于 22:00 重启\n",
            Path::new("x"),
        )
        .unwrap();
        assert_eq!(cfg.title, "我的 网盘");
        assert_eq!(cfg.notice, "服务器将于 22:00 重启", "unquoted spaces are kept");

        // A '#' starts a comment unless quoted.
        apply_conf_text(&mut cfg, "Fern.Notice \"#1 维护中\"", Path::new("x")).unwrap();
        assert_eq!(cfg.notice, "#1 维护中");

        assert!(apply_conf_text(&mut cfg, "Fern.Title", Path::new("x")).is_err());
        let too_long = format!("Fern.Title \"{}\"", "x".repeat(MAX_BRAND_CHARS + 1));
        assert!(apply_conf_text(&mut cfg, &too_long, Path::new("x")).is_err());
    }

    #[test]
    fn log_level_round_trips_through_rank() {
        for level in [
            LogLevel::Error,
            LogLevel::Warn,
            LogLevel::Info,
            LogLevel::Debug,
        ] {
            assert_eq!(LogLevel::from_rank(level.rank()), level);
            assert_eq!(LogLevel::parse(level.as_str()), Some(level));
        }
        assert_eq!(LogLevel::parse("INFO"), Some(LogLevel::Info));
        assert_eq!(LogLevel::parse("verbose"), None);
        assert_eq!(LogLevel::default(), LogLevel::Info);
        assert_eq!(LogLevel::Error.tag(), "ERROR");
    }

    #[test]
    fn log_level_directive_filters_through_defaults() {
        let mut cfg = Config::default();
        assert_eq!(cfg.log_level, LogLevel::Info);
        apply_conf_text(&mut cfg, "Fern.LogLevel debug", Path::new("x")).unwrap();
        assert_eq!(cfg.log_level, LogLevel::Debug);
        assert!(apply_conf_text(&mut cfg, "Fern.LogLevel loud", Path::new("x")).is_err());
    }

    #[test]
    fn quote_value_keeps_one_token() {
        assert_eq!(quote_value("4545"), "4545");
        assert_eq!(quote_value("/data/my share"), "\"/data/my share\"");
        assert_eq!(quote_value("#tag"), "\"#tag\"");
        assert_eq!(quote_value(""), "\"\"");
        assert_eq!(split_tokens(&quote_value("a b")).len(), 1);
    }

    #[test]
    fn merge_replaces_owned_directives_and_keeps_everything_else() {
        let original = "\
# 顶部注释
Fern.Root /old
Fern.Listen 1
Fern.Listen 2
Fern.User RLUM alice secret
Fern.Guest RL

# 回收站
Trash.Max.Size 256
";
        let blocks = vec![
            ("fern.root", vec!["Fern.Root \"/new share\"".to_string()]),
            ("fern.listen", vec!["Fern.Listen 4545".to_string()]),
            ("trash.enabled", vec!["Trash.Enabled 1".to_string()]),
            ("fern.notice", Vec::new()),
        ];
        let merged = merge_directives(original, &blocks);
        assert!(merged.contains("# 顶部注释"));
        assert!(merged.contains("Fern.Root \"/new share\""));
        assert!(!merged.contains("Fern.Root /old"));
        assert!(merged.contains("Fern.Listen 4545"));
        assert!(!merged.contains("Fern.Listen 1"));
        assert!(!merged.contains("Fern.Listen 2"));
        assert!(merged.contains("Fern.User RLUM alice secret"));
        assert!(merged.contains("Fern.Guest RL"));
        assert!(merged.contains("# 回收站"));
        assert!(merged.contains("Trash.Max.Size 256"));
        assert!(merged.ends_with('\n'));

        // The merged text must parse again, and a second pass is a no-op.
        let cfg = Config::from_text(&merged, Path::new("fern.conf")).unwrap();
        assert_eq!(cfg.listen, vec!["4545".to_string()]);
        assert_eq!(merge_directives(&merged, &blocks), merged);
    }

    #[test]
    fn merge_drops_emptied_directives_and_appends_missing_ones() {
        let original = "Fern.Root /share\nFern.Notice hello\n";
        let blocks = vec![
            ("fern.notice", Vec::new()),
            ("fern.loglevel", vec!["Fern.LogLevel Debug".to_string()]),
        ];
        let merged = merge_directives(original, &blocks);
        assert!(!merged.contains("Fern.Notice"));
        assert!(merged.contains("Fern.Root /share"));
        assert!(merged.contains("Fern.LogLevel Debug"));
        let cfg = Config::from_text(&merged, Path::new("fern.conf")).unwrap();
        assert_eq!(cfg.notice, "");
        assert_eq!(cfg.log_level, LogLevel::Debug);
    }

    #[test]
    fn merge_keeps_crlf_files_in_crlf() {
        let original = "Fern.Root /old\r\nFern.Listen 1\r\n# 注释\r\n";
        let blocks = vec![("fern.root", vec!["Fern.Root /new".to_string()])];
        let merged = merge_directives(original, &blocks);
        assert_eq!(merged, "Fern.Root /new\r\nFern.Listen 1\r\n# 注释\r\n");
        assert!(!merged.contains("\n\n"));
    }

    #[test]
    fn editable_blocks_survive_a_merge_round_trip() {
        let mut cfg = Config::default();
        cfg.root = PathBuf::from("/data/my share");
        cfg.listen = vec!["4545".to_string(), "[::]:9000".to_string()];
        cfg.log_level = LogLevel::Debug;
        cfg.log_dir = Some(PathBuf::from("C:\\logs dir"));
        cfg.title = "我的 网盘".to_string();
        cfg.guest = 0;
        let text = merge_directives("", &cfg.editable_blocks());
        let again = Config::from_text(&text, Path::new("fern.conf")).unwrap();
        assert_eq!(again.listen, cfg.listen);
        assert_eq!(again.log_level, cfg.log_level);
        assert_eq!(again.title, cfg.title);
        assert_eq!(again.guest, cfg.guest);
        assert_eq!(again.log_dir, cfg.log_dir);
    }
}
