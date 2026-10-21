//! Content type lookup. A small built-in table plus an optional `fern.mime`
//! override file (`<ext> <type>` or `<type> <ext> <ext>...`).

use std::path::Path;
use std::sync::OnceLock;

static OVERRIDES: OnceLock<Vec<(String, String)>> = OnceLock::new();

pub fn load_overrides(paths: &[&Path]) {
    let mut table: Vec<(String, String)> = Vec::new();
    for p in paths {
        let Ok(text) = std::fs::read_to_string(p) else {
            continue;
        };
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let first = match parts.next() {
                Some(v) => v,
                None => continue,
            };
            let rest: Vec<&str> = parts.collect();
            if first.contains('/') {
                for ext in rest {
                    table.push((ext.trim_start_matches('.').to_ascii_lowercase(), first.to_string()));
                }
            } else if !rest.is_empty() {
                table.push((first.trim_start_matches('.').to_ascii_lowercase(), rest[0].to_string()));
            }
        }
    }
    let _ = OVERRIDES.set(table);
}

pub fn for_path(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if let Some(table) = OVERRIDES.get() {
        for (e, t) in table {
            if *e == ext {
                return t.clone();
            }
        }
    }
    builtin(&ext).to_string()
}

pub fn builtin(ext: &str) -> &'static str {
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "txt" | "log" | "ini" | "conf" | "cfg" | "md" | "markdown" => "text/plain; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "heic" => "image/heic",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "wav" => "audio/wav",
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "ts" => "video/mp2t",
        "flv" => "video/x-flv",
        "wmv" => "video/x-ms-wmv",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "bz2" => "application/x-bzip2",
        "xz" => "application/x-xz",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "tar" => "application/x-tar",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "apk" => "application/vnd.android.package-archive",
        "exe" => "application/vnd.microsoft.portable-executable",
        "msi" => "application/x-msi",
        "iso" => "application/x-iso9660-image",
        _ => "application/octet-stream",
    }
}

/// True when the browser can render the file inline for a preview.
pub fn is_previewable(content_type: &str) -> bool {
    content_type.starts_with("image/")
        || content_type.starts_with("video/")
        || content_type.starts_with("audio/")
        || content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("pdf")
}
