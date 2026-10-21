//! JSON API handlers used by the bundled UI.

use crate::auth::{self, Identity, Sessions};
use crate::config::{
    perms_to_letters, Config, PERM_LIST, PERM_MODIFY, PERM_READ, PERM_SHOW_HIDDEN, PERM_UPLOAD,
};
use crate::fsapi::{self, FsError};
use crate::http::{Request, Response};
use crate::json::{self, Json};
use crate::trash;
use std::io::Read;
use std::path::Path;
use std::time::UNIX_EPOCH;

const MAX_JSON_BODY: u64 = 4 * 1024 * 1024;
/// Text files at or below this size can be opened in the browser editor.
const MAX_TEXT_BYTES: u64 = 2 * 1024 * 1024;
/// The write body is JSON, so escaping can make it larger than the file.
const MAX_WRITE_BODY: u64 = 16 * 1024 * 1024;

fn letters_to_array(perms: u32) -> Json {
    Json::Arr(
        perms_to_letters(perms)
            .chars()
            .map(|c| Json::Str(c.to_string()))
            .collect(),
    )
}

fn err_response(e: &FsError) -> Response {
    let status = match e {
        FsError::Io(ref io) if io.kind() == std::io::ErrorKind::AlreadyExists => 409,
        FsError::Io(ref io) if io.kind() == std::io::ErrorKind::NotFound => 404,
        other => other.status(),
    };
    Response::error(status, e.code(), &e.message())
}

pub fn session_info(cfg: &Config, id: &Identity) -> Response {
    let body = json::obj(vec![
        ("user", Json::Str(id.name.clone())),
        ("loggedIn", Json::Bool(id.logged_in)),
        ("guest", Json::Bool(!id.logged_in)),
        ("letters", Json::Str(perms_to_letters(id.perms))),
        ("perms", letters_to_array(id.perms)),
        ("trashEnabled", Json::Bool(cfg.trash_enabled)),
        ("title", Json::Str(cfg.title.clone())),
        ("notice", Json::Str(cfg.notice.clone())),
    ])
    .dump();
    Response::json(200, body)
}

pub fn login(
    cfg: &Config,
    sessions: &Sessions,
    req: &Request,
    body: &str,
    remote: &str,
) -> Response {
    let parsed = match json::parse(body) {
        Ok(v) => v,
        Err(_) => return Response::error(400, "bad_json", "request body must be JSON"),
    };
    let user = parsed.get("user").and_then(|v| v.as_str()).unwrap_or("");
    let pass = parsed.get("pass").and_then(|v| v.as_str()).unwrap_or("");
    if user.is_empty() || pass.is_empty() {
        return Response::error(400, "missing_credentials", "user and pass are required");
    }
    let key = format!("{}|{}", remote, user);
    if !sessions.allow_attempt(&key) {
        return Response::error(429, "too_many_attempts", "too many failed logins");
    }
    match auth::authenticate(cfg, user, pass) {
        Some(account) => {
            sessions.clear_failures(&key);
            let token = sessions.create(&account.name, account.perms);
            let id = Identity {
                name: account.name.clone(),
                perms: account.perms,
                logged_in: true,
                token: Some(token.clone()),
            };
            let mut resp = session_response(cfg, &id);
            resp.headers.push((
                "Set-Cookie".into(),
                format!(
                    "{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
                    auth::SESSION_COOKIE,
                    token,
                    auth::SESSION_TTL
                ),
            ));
            resp.headers.push(("Cache-Control".into(), "no-store".into()));
            let _ = req;
            resp
        }
        None => {
            sessions.note_failure(&key);
            Response::error(401, "bad_credentials", "wrong user name or password")
        }
    }
}

fn session_response(cfg: &Config, id: &Identity) -> Response {
    session_info(cfg, id)
}

pub fn logout(sessions: &Sessions, id: &Identity) -> Response {
    if let Some(token) = &id.token {
        sessions.remove(token);
    }
    let mut resp = Response::json(
        200,
        json::obj(vec![("ok", Json::Bool(true))]).dump(),
    );
    resp.headers.push((
        "Set-Cookie".into(),
        format!(
            "{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
            auth::SESSION_COOKIE
        ),
    ));
    resp
}

pub fn list(cfg: &Config, id: &Identity, req: &Request) -> Response {
    if !id.can(PERM_LIST) {
        return Response::error(403, "forbidden", "listing is not permitted");
    }
    let requested = req.q("path").unwrap_or("/");
    let dir = match fsapi::resolve(&cfg.root, requested) {
        Ok(p) => p,
        Err(e) => return err_response(&e),
    };
    if let Err(e) = fsapi::ensure_inside(&cfg.root, &dir) {
        return err_response(&e);
    }
    // The recycle bin is deliberately unreachable through the file API; it has
    // its own endpoints so entries can never be edited or downloaded in place.
    if dir.starts_with(trash::dir(&cfg.root)) {
        return Response::error(
            403,
            "forbidden",
            "the recycle bin is not browsable here",
        );
    }
    let meta = match std::fs::metadata(&dir) {
        Ok(m) => m,
        Err(_) => return Response::error(404, "not_found", "directory not found"),
    };
    if !meta.is_dir() {
        return Response::error(400, "not_a_directory", "path is not a directory");
    }
    let show_hidden = id.can(PERM_SHOW_HIDDEN);
    let entries = match fsapi::list_dir(&cfg.root, &dir, show_hidden) {
        Ok(e) => e,
        Err(e) => return err_response(&e),
    };
    let items: Vec<Json> = entries
        .iter()
        .map(|e| {
            json::obj(vec![
                ("name", Json::Str(e.name.clone())),
                ("isDir", Json::Bool(e.is_dir)),
                ("size", Json::Num(e.size as i64)),
                ("mtime", Json::Num(e.mtime as i64)),
                ("mode", Json::Num(e.mode as i64)),
                ("hidden", Json::Bool(e.hidden)),
                ("link", Json::Bool(e.is_link)),
            ])
        })
        .collect();
    let body = json::obj(vec![
        ("path", Json::Str(fsapi::relative(&cfg.root, &dir))),
        ("entries", Json::Arr(items)),
        ("writable", Json::Bool(id.can(PERM_UPLOAD) || id.can(PERM_MODIFY))),
        ("upload", Json::Bool(id.can(PERM_UPLOAD))),
        ("modify", Json::Bool(id.can(PERM_MODIFY))),
        ("letters", Json::Str(perms_to_letters(id.perms))),
    ])
    .dump();
    Response::json(200, body)
}

fn read_json_body<R: Read>(req: &Request, reader: &mut R) -> Result<Json, Response> {
    read_json_body_limit(req, reader, MAX_JSON_BODY)
}

fn read_json_body_limit<R: Read>(
    req: &Request,
    reader: &mut R,
    limit: u64,
) -> Result<Json, Response> {
    let len = match req.content_length() {
        Some(v) if v <= limit => v,
        Some(_) => {
            return Err(Response::error(
                413,
                "too_large",
                "request body exceeds the limit",
            ))
        }
        None => 0,
    };
    if len == 0 {
        return json::parse("{}")
            .map_err(|_| Response::error(400, "bad_json", "request body must be JSON"));
    }
    let mut buf = vec![0u8; len as usize];
    reader
        .read_exact(&mut buf)
        .map_err(|_| Response::error(400, "bad_body", "cannot read request body"))?;
    let text = String::from_utf8(buf)
        .map_err(|_| Response::error(400, "bad_body", "request body must be UTF-8"))?;
    json::parse(&text).map_err(|_| Response::error(400, "bad_json", "request body must be JSON"))
}

/// Consumes the rest of a request body before an early error response. Closing
/// a socket that still holds unread body bytes can reset the connection, which
/// the browser reports as an opaque network error instead of the JSON message.
fn discard_body<R: Read>(reader: &mut R, req: &Request) {
    let mut left = req.content_length().unwrap_or(0).min(64 * 1024 * 1024);
    if left == 0 {
        return;
    }
    let mut buf = [0u8; 16 * 1024];
    while left > 0 {
        let want = left.min(buf.len() as u64) as usize;
        match reader.read(&mut buf[..want]) {
            Ok(0) => return,
            Ok(n) => left -= n as u64,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

fn target_of(cfg: &Config, dir: &str, name: &str) -> Result<std::path::PathBuf, Response> {
    if name.is_empty() || name.starts_with('/') || name.contains('\0') {
        return Err(Response::error(400, "bad_name", "invalid name"));
    }
    let joined = format!("{}/{}", dir.trim_end_matches('/'), name);
    let path = fsapi::resolve(&cfg.root, &joined).map_err(|e| err_response(&e))?;
    fsapi::ensure_inside(&cfg.root, &path).map_err(|e| err_response(&e))?;
    Ok(path)
}

pub fn mkdir<R: Read>(
    cfg: &Config,
    id: &Identity,
    req: &Request,
    reader: &mut R,
) -> Response {
    if !id.can(PERM_MODIFY) {
        return Response::error(403, "forbidden", "creating folders is not permitted");
    }
    let body = match read_json_body(req, reader) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let dir = body.get("path").and_then(|v| v.as_str()).unwrap_or("/");
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if name.contains('/') || name.contains('\\') {
        return Response::error(400, "bad_name", "name must not contain separators");
    }
    let target = match target_of(cfg, dir, name) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match fsapi::create_dir(&cfg.root, &target) {
        Ok(()) => Response::json(
            200,
            json::obj(vec![("ok", Json::Bool(true))]).dump(),
        ),
        Err(e) => err_response(&e),
    }
}

pub fn new_file<R: Read>(
    cfg: &Config,
    id: &Identity,
    req: &Request,
    reader: &mut R,
) -> Response {
    if !id.can(PERM_MODIFY) {
        return Response::error(403, "forbidden", "creating files is not permitted");
    }
    let body = match read_json_body(req, reader) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let dir = body.get("path").and_then(|v| v.as_str()).unwrap_or("/");
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if name.contains('/') || name.contains('\\') {
        return Response::error(400, "bad_name", "name must not contain separators");
    }
    if name == "." || name == ".." {
        return Response::error(400, "bad_name", "invalid name");
    }
    if fsapi::is_internal_name(name) || name == fsapi::TRASH_DIR {
        return Response::error(400, "bad_name", "this name is reserved");
    }
    let target = match target_of(cfg, dir, name) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if target.exists() {
        return Response::error(409, "exists", "this name is already taken");
    }
    match fsapi::create_file(&cfg.root, &target) {
        Ok(()) => Response::json(
            200,
            json::obj(vec![
                ("ok", Json::Bool(true)),
                ("path", Json::Str(fsapi::relative(&cfg.root, &target))),
                ("name", Json::Str(name.to_string())),
            ])
            .dump(),
        ),
        Err(e) => err_response(&e),
    }
}

pub fn rename<R: Read>(
    cfg: &Config,
    id: &Identity,
    req: &Request,
    reader: &mut R,
) -> Response {
    if !id.can(PERM_MODIFY) {
        return Response::error(403, "forbidden", "renaming is not permitted");
    }
    let body = match read_json_body(req, reader) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let from = body.get("from").and_then(|v| v.as_str()).unwrap_or("");
    let to = body.get("to").and_then(|v| v.as_str()).unwrap_or("");
    if from.is_empty() || to.is_empty() || from == "/" {
        return Response::error(400, "bad_path", "from and to are required");
    }
    let from_path = match fsapi::resolve(&cfg.root, from) {
        Ok(p) => p,
        Err(e) => return err_response(&e),
    };
    let to_path = match fsapi::resolve(&cfg.root, to) {
        Ok(p) => p,
        Err(e) => return err_response(&e),
    };
    match fsapi::rename(&cfg.root, &from_path, &to_path) {
        Ok(()) => Response::json(
            200,
            json::obj(vec![("ok", Json::Bool(true))]).dump(),
        ),
        Err(e) => err_response(&e),
    }
}

/// Appends ` - 副本`, before the extension of a file, until the name is free
/// inside `dir`. `a.txt` → `a - 副本.txt` → `a - 副本 (2).txt` → …
fn sibling_name(dir: &Path, name: &str, is_dir: bool) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(idx) if !is_dir && idx > 0 => (&name[..idx], &name[idx..]),
        _ => (name, ""),
    };
    let mut candidate = format!("{} - 副本{}", stem, ext);
    let mut n = 2;
    while dir.join(&candidate).exists() {
        candidate = format!("{} - 副本 ({}){}", stem, n, ext);
        n += 1;
    }
    candidate
}

/// Copies entries into a directory, giving every copy a distinct name. The
/// frontend uses this for copy/paste.
pub fn copy<R: Read>(
    cfg: &Config,
    id: &Identity,
    req: &Request,
    reader: &mut R,
) -> Response {
    if !id.can(PERM_MODIFY) {
        return Response::error(403, "forbidden", "copying is not permitted");
    }
    let body = match read_json_body(req, reader) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let sources = match body.get("paths").and_then(|v| v.as_arr()) {
        Some(v) if !v.is_empty() => v,
        _ => return Response::error(400, "bad_request", "paths must be a non empty array"),
    };
    let dest = body.get("dest").and_then(|v| v.as_str()).unwrap_or("/");
    let dest_path = match fsapi::resolve(&cfg.root, dest) {
        Ok(p) => p,
        Err(e) => return err_response(&e),
    };
    if !dest_path.is_dir() {
        return Response::error(400, "not_directory", "destination is not a directory");
    }
    let bin = trash::dir(&cfg.root);
    let mut copied = 0u32;
    let mut targets: Vec<Json> = Vec::new();
    let mut last_error: Option<Response> = None;
    for item in sources {
        let rel = match item.as_str() {
            Some(s) if !s.is_empty() && s != "/" => s,
            _ => continue,
        };
        let path = match fsapi::resolve(&cfg.root, rel) {
            Ok(p) => p,
            Err(e) => {
                last_error = Some(err_response(&e));
                continue;
            }
        };
        let name = match path.file_name() {
            Some(n) => n.to_string_lossy().into_owned(),
            None => continue,
        };
        // The bin and its bookkeeping files are never copied.
        if path.starts_with(&bin) || fsapi::is_internal_name(&name) || name == fsapi::TRASH_DIR {
            last_error = Some(Response::error(403, "forbidden", "this entry cannot be copied"));
            continue;
        }
        let is_dir = std::fs::symlink_metadata(&path).map(|m| m.is_dir()).unwrap_or(false);
        if is_dir && dest_path.starts_with(&path) {
            last_error = Some(Response::error(
                400,
                "copy_into_self",
                "cannot copy a folder into itself or one of its subfolders",
            ));
            continue;
        }
        let target = dest_path.join(sibling_name(&dest_path, &name, is_dir));
        match fsapi::copy_entry(&cfg.root, &path, &target) {
            Ok(()) => {
                copied += 1;
                targets.push(Json::Str(fsapi::relative(&cfg.root, &target)));
            }
            Err(e) => last_error = Some(err_response(&e)),
        }
    }
    if copied == 0 {
        if let Some(r) = last_error {
            return r;
        }
    }
    Response::json(
        200,
        json::obj(vec![
            ("copied", Json::Num(copied as i64)),
            ("paths", Json::Arr(targets)),
        ])
        .dump(),
    )
}

pub fn delete<R: Read>(
    cfg: &Config,
    id: &Identity,
    req: &Request,
    reader: &mut R,
) -> Response {
    if !id.can(PERM_MODIFY) {
        return Response::error(403, "forbidden", "deleting is not permitted");
    }
    let body = match read_json_body(req, reader) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let list = match body.get("paths").and_then(|v| v.as_arr()) {
        Some(v) if !v.is_empty() => v,
        _ => return Response::error(400, "bad_request", "paths must be a non empty array"),
    };
    let mut moved = 0u32;
    let mut last_error: Option<Response> = None;
    for item in list {
        let rel = match item.as_str() {
            Some(s) if !s.is_empty() && s != "/" => s,
            _ => continue,
        };
        let path = match fsapi::resolve(&cfg.root, rel) {
            Ok(p) => p,
            Err(e) => {
                last_error = Some(err_response(&e));
                continue;
            }
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Half finished uploads and edits are useless once abandoned, so they
        // are dropped outright instead of being kept in the recycle bin.
        let permanent = !cfg.trash_enabled || fsapi::is_internal_name(&name);
        let outcome = if permanent {
            fsapi::remove_entry(&path)
        } else {
            trash::move_to_trash(&cfg.root, &path, &id.name).map(|_| ())
        };
        match outcome {
            Ok(()) => moved += 1,
            Err(e) => last_error = Some(err_response(&e)),
        }
    }
    if moved > 0 && cfg.trash_enabled {
        trash::cleanup(&cfg.root, &cfg.trash_policy());
    }
    if moved == 0 {
        if let Some(r) = last_error {
            return r;
        }
    }
    Response::json(
        200,
        json::obj(vec![
            ("moved", Json::Num(moved as i64)),
            ("permanent", Json::Bool(!cfg.trash_enabled)),
        ])
        .dump(),
    )
}

/// Recycle bin listing. `.Trash` itself stays unreachable through `/_api/list`
/// so the bin is only ever browsed through this endpoint.
pub fn trash_list(cfg: &Config, id: &Identity, req: &Request) -> Response {
    if !id.can(PERM_MODIFY) {
        return Response::error(403, "forbidden", "the recycle bin needs the modify permission");
    }
    let mut items = trash::list(&cfg.root);
    let bytes: u64 = items.iter().map(|e| e.size).sum();
    let total = items.len();
    match req.q("sort").as_deref() {
        Some("oldest") => items.sort_by(|a, b| a.deleted_at.cmp(&b.deleted_at)),
        Some("name") => items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
        _ => {}
    }
    let offset = req
        .q("offset")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = req
        .q("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(500)
        .clamp(1, 2000);
    let page: Vec<Json> = items
        .iter()
        .skip(offset)
        .take(limit)
        .map(|e| e.to_json())
        .collect();
    let body = json::obj(vec![
        ("items", Json::Arr(page)),
        ("total", Json::Num(total as i64)),
        ("bytes", Json::Num(bytes as i64)),
        ("offset", Json::Num(offset as i64)),
        ("limit", Json::Num(limit as i64)),
        ("enabled", Json::Bool(cfg.trash_enabled)),
        ("maxDays", Json::Num(cfg.trash_max_days as i64)),
        ("maxBytes", Json::Num(cfg.trash_max_mb.saturating_mul(1024 * 1024) as i64)),
        ("keepSame", Json::Num(cfg.trash_keep_same as i64)),
    ])
    .dump();
    Response::json(200, body)
}

fn id_list(body: &Json) -> Vec<String> {
    match body.get("ids").and_then(|v| v.as_arr()) {
        Some(list) => list
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect(),
        None => Vec::new(),
    }
}

pub fn trash_restore<R: Read>(
    cfg: &Config,
    id: &Identity,
    req: &Request,
    reader: &mut R,
) -> Response {
    if !id.can(PERM_MODIFY) {
        discard_body(reader, req);
        return Response::error(403, "forbidden", "the recycle bin needs the modify permission");
    }
    let body = match read_json_body(req, reader) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let ids = id_list(&body);
    if ids.is_empty() {
        return Response::error(400, "bad_request", "ids must be a non empty array");
    }
    let overwrite = body
        .get("overwrite")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut restored = 0u32;
    let mut paths: Vec<Json> = Vec::new();
    let mut failed: Vec<Json> = Vec::new();
    let mut conflict = false;
    for entry_id in &ids {
        match trash::restore(&cfg.root, entry_id, overwrite) {
            Ok(path) => {
                restored += 1;
                paths.push(Json::Str(path));
            }
            Err(e) => {
                if e.code == "exists" {
                    conflict = true;
                }
                failed.push(json::obj(vec![
                    ("id", Json::Str(entry_id.clone())),
                    ("code", Json::Str(e.code.into())),
                    ("path", Json::Str(e.path.clone())),
                    ("message", Json::Str(e.message.clone())),
                ]));
            }
        }
    }
    let payload = json::obj(vec![
        ("restored", Json::Num(restored as i64)),
        ("paths", Json::Arr(paths)),
        ("failed", Json::Arr(failed)),
    ])
    .dump();
    if restored == 0 && conflict {
        return Response::json(409, payload);
    }
    Response::json(200, payload)
}

pub fn trash_purge<R: Read>(
    cfg: &Config,
    id: &Identity,
    req: &Request,
    reader: &mut R,
) -> Response {
    if !id.can(PERM_MODIFY) {
        discard_body(reader, req);
        return Response::error(403, "forbidden", "the recycle bin needs the modify permission");
    }
    let body = match read_json_body(req, reader) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let ids = id_list(&body);
    if ids.is_empty() {
        return Response::error(400, "bad_request", "ids must be a non empty array");
    }
    let mut purged = 0u32;
    for entry_id in &ids {
        if trash::purge(&cfg.root, entry_id) {
            purged += 1;
        }
    }
    Response::json(
        200,
        json::obj(vec![("purged", Json::Num(purged as i64))]).dump(),
    )
}

pub fn trash_empty<R: Read>(
    cfg: &Config,
    id: &Identity,
    req: &Request,
    reader: &mut R,
) -> Response {
    if !id.can(PERM_MODIFY) {
        discard_body(reader, req);
        return Response::error(403, "forbidden", "the recycle bin needs the modify permission");
    }
    let body = match read_json_body(req, reader) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let days = body
        .get("olderThanDays")
        .and_then(|v| v.as_num())
        .filter(|n| *n >= 0)
        .map(|n| n as u64);
    let purged = trash::empty(&cfg.root, days);
    Response::json(
        200,
        json::obj(vec![("purged", Json::Num(purged as i64))]).dump(),
    )
}

/// Streams an upload chunk. `offset` positions the write inside `<name>.fernpart`.
pub fn upload<R: Read>(
    cfg: &Config,
    id: &Identity,
    req: &Request,
    reader: &mut R,
) -> Response {
    if !id.can(PERM_UPLOAD) {
        discard_body(reader, req);
        return Response::error(403, "forbidden", "uploading is not permitted");
    }
    let dir = req.q("path").unwrap_or("/");
    let name = req.q("name").unwrap_or("");
    let offset: u64 = req.q("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
    let total: u64 = req.q("total").and_then(|v| v.parse().ok()).unwrap_or(0);
    let restart = matches!(req.q("restart"), Some("1") | Some("true"));
    let final_path = match target_of(cfg, dir, name) {
        Ok(p) => p,
        Err(r) => {
            discard_body(reader, req);
            return r;
        }
    };
    if final_path.is_dir() {
        discard_body(reader, req);
        return Response::error(409, "is_directory", "target is a directory");
    }
    let len = match req.content_length() {
        Some(v) => v,
        None => {
            discard_body(reader, req);
            return Response::error(411, "length_required", "Content-Length is required");
        }
    };
    if restart && offset != 0 {
        discard_body(reader, req);
        return Response::error(400, "bad_offset", "restart requires offset=0");
    }
    if total != 0 && offset + len > total {
        discard_body(reader, req);
        return Response::error(400, "bad_offset", "chunk overflows the declared size");
    }
    let have = fsapi::existing_partial(&final_path);
    if !restart && have != offset {
        // A previous attempt left data behind. Report where to resume rather
        // than silently truncating it.
        let body = json::obj(vec![
            ("error", Json::Str("offset_mismatch".into())),
            ("offset", Json::Num(have as i64)),
            ("total", Json::Num(total as i64)),
            (
                "message",
                Json::Str("server holds a different amount of data".into()),
            ),
        ])
        .dump();
        discard_body(reader, req);
        return Response::json(409, body);
    }

    let mut writer = match fsapi::UploadWriter::open(&final_path, offset, restart) {
        Ok(w) => w,
        Err(e) => {
            discard_body(reader, req);
            return err_response(&e);
        }
    };
    let mut remaining = len;
    let mut buf = vec![0u8; 64 * 1024];
    while remaining > 0 {
        let want = remaining.min(buf.len() as u64) as usize;
        match reader.read(&mut buf[..want]) {
            Ok(0) => break,
            Ok(n) => {
                if let Err(e) = writer.write_chunk(&buf[..n]) {
                    // Keep the partial data so a later request can resume.
                    discard_body(reader, req);
                    return err_response(&e);
                }
                remaining -= n as u64;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                discard_body(reader, req);
                return Response::error(400, "incomplete_body", "upload interrupted");
            }
        }
    }
    let written = offset + writer.written;
    let complete = total == 0 || written >= total;
    if remaining > 0 {
        discard_body(reader, req);
        return Response::error(400, "incomplete_body", "request body was shorter than declared");
    }
    if complete {
        if let Err(e) = writer.finish(&final_path) {
            return err_response(&e);
        }
    }
    Response::json(
        200,
        json::obj(vec![
            ("offset", Json::Num(written as i64)),
            ("done", Json::Bool(complete)),
            ("size", Json::Num(written as i64)),
        ])
        .dump(),
    )
}

/// Reports how much of a previous upload attempt the server still holds, so a
/// reloaded page can resume instead of starting over.
pub fn upload_probe(cfg: &Config, id: &Identity, req: &Request) -> Response {
    if !id.can(PERM_UPLOAD) {
        return Response::error(403, "forbidden", "uploading is not permitted");
    }
    let dir = req.q("path").unwrap_or("/");
    let name = req.q("name").unwrap_or("");
    if name.is_empty() {
        return Response::error(400, "bad_name", "name is required");
    }
    let final_path = match target_of(cfg, dir, name) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let body = json::obj(vec![
        (
            "offset",
            Json::Num(fsapi::existing_partial(&final_path) as i64),
        ),
        ("exists", Json::Bool(final_path.is_file())),
    ])
    .dump();
    Response::json(200, body)
}

/// Drops the leftover partial data of an upload the user cancelled.
pub fn upload_cancel(cfg: &Config, id: &Identity, req: &Request) -> Response {
    if !id.can(PERM_UPLOAD) {
        return Response::error(403, "forbidden", "uploading is not permitted");
    }
    let dir = req.q("path").unwrap_or("/");
    let name = req.q("name").unwrap_or("");
    if name.is_empty() {
        return Response::error(400, "bad_name", "name is required");
    }
    let final_path = match target_of(cfg, dir, name) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match fsapi::discard_partial(&final_path) {
        Ok(()) => Response::json(200, json::obj(vec![("ok", Json::Bool(true))]).dump()),
        Err(e) => err_response(&e),
    }
}

fn file_of(cfg: &Config, rel: &str) -> Result<std::path::PathBuf, Response> {
    if rel.is_empty() || rel == "/" {
        return Err(Response::error(400, "bad_path", "path is required"));
    }
    let path = fsapi::resolve(&cfg.root, rel).map_err(|e| err_response(&e))?;
    fsapi::ensure_inside(&cfg.root, &path).map_err(|e| err_response(&e))?;
    Ok(path)
}

fn mtime_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Returns a UTF-8 text file together with the metadata the editor needs.
pub fn read_text(cfg: &Config, id: &Identity, req: &Request) -> Response {
    if !id.can(PERM_READ) {
        return Response::error(403, "forbidden", "reading files is not permitted");
    }
    let rel = req.q("path").unwrap_or("");
    let path = match file_of(cfg, rel) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(_) => return Response::error(404, "not_found", "file not found"),
    };
    if meta.is_dir() {
        return Response::error(400, "is_directory", "path is a directory");
    }
    if meta.len() > MAX_TEXT_BYTES {
        return Response::error(413, "too_large", "file is too large to edit in the browser");
    }
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => return err_response(&FsError::from(e)),
    };
    let (bom, data) = match bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        Some(rest) => (true, rest),
        None => (false, bytes.as_slice()),
    };
    if data.contains(&0) {
        return Response::error(415, "not_text", "file is not UTF-8 text");
    }
    let text = match std::str::from_utf8(data) {
        Ok(t) => t,
        Err(_) => return Response::error(415, "not_text", "file is not UTF-8 text"),
    };
    let body = json::obj(vec![
        ("path", Json::Str(fsapi::relative(&cfg.root, &path))),
        ("content", Json::Str(text.to_string())),
        ("size", Json::Num(meta.len() as i64)),
        ("mtimeMs", Json::Num(mtime_ms(&meta) as i64)),
        ("bom", Json::Bool(bom)),
        ("canEdit", Json::Bool(id.can(PERM_MODIFY))),
        ("maxBytes", Json::Num(MAX_TEXT_BYTES as i64)),
    ])
    .dump();
    Response::json(200, body)
}

/// Saves editor content. `mtimeMs` from [`read_text`] rejects stale overwrites
/// with `409 conflict` unless the client explicitly sends `force`.
pub fn write_text<R: Read>(
    cfg: &Config,
    id: &Identity,
    req: &Request,
    reader: &mut R,
) -> Response {
    if !id.can(PERM_MODIFY) {
        return Response::error(403, "forbidden", "editing files is not permitted");
    }
    let body = match read_json_body_limit(req, reader, MAX_WRITE_BODY) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let rel = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let content = match body.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return Response::error(400, "bad_request", "content must be a string"),
    };
    if content.len() as u64 > MAX_TEXT_BYTES {
        return Response::error(413, "too_large", "content exceeds the edit limit");
    }
    let expected = body
        .get("mtimeMs")
        .and_then(|v| v.as_num())
        .filter(|n| *n >= 0)
        .map(|n| n as u64);
    let force = body.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let bom = body.get("bom").and_then(|v| v.as_bool()).unwrap_or(false);

    let path = match file_of(cfg, rel) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(_) => return Response::error(404, "not_found", "file not found"),
    };
    if meta.is_dir() {
        return Response::error(409, "is_directory", "target is a directory");
    }
    let current = mtime_ms(&meta);
    if !force {
        if let Some(exp) = expected {
            if exp != current {
                let body = json::obj(vec![
                    ("error", Json::Str("conflict".into())),
                    (
                        "message",
                        Json::Str("the file changed on the server".into()),
                    ),
                    ("mtimeMs", Json::Num(current as i64)),
                ])
                .dump();
                return Response::json(409, body);
            }
        }
    }

    let mut data = Vec::with_capacity(content.len() + 3);
    if bom {
        data.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    data.extend_from_slice(content.as_bytes());
    if let Err(e) = fsapi::replace_contents(&path, &data) {
        return err_response(&e);
    }
    let updated = std::fs::metadata(&path).ok();
    let size = updated
        .as_ref()
        .map(|m| m.len())
        .unwrap_or(data.len() as u64);
    let stamp = updated.as_ref().map(mtime_ms).unwrap_or(current);
    Response::json(
        200,
        json::obj(vec![
            ("ok", Json::Bool(true)),
            ("size", Json::Num(size as i64)),
            ("mtimeMs", Json::Num(stamp as i64)),
        ])
        .dump(),
    )
}
