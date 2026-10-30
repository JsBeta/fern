//! Listener setup, connection handling and request routing.

use crate::api;
use crate::assets;
use crate::auth::{self, Identity, Sessions};
use crate::config::{Config, PERM_READ};
use crate::fsapi;
use crate::http::{self, Body, Request, Response};
use crate::mime;
use crate::range::{self, RangeSpec};
use crate::util::{self, percent_encode};
use crate::zip::{self, ZipStream};
use std::io::{self, BufReader};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const CSP: &str = "default-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; \
style-src 'self' 'unsafe-inline'; script-src 'self'; object-src 'none'; base-uri 'none'; \
form-action 'self'; frame-ancestors 'none'";

/// Upper bound on the requests served over one connection before it is closed.
const MAX_KEEPALIVE_REQUESTS: u32 = 100;

pub struct Server {
    pub cfg: Arc<Config>,
    pub listeners: Vec<TcpListener>,
    pub sessions: Sessions,
    active: AtomicUsize,
    pub shutdown: Arc<AtomicUsize>,
}

pub fn bind_all(cfg: &Config) -> io::Result<Vec<TcpListener>> {
    bind_all_retry(cfg, 0, Duration::from_millis(0))
}

/// Binds every configured address, retrying `retries` times with `delay`
/// between attempts while a port is still in use.
///
/// A tray restart spawns the replacement before the old process is gone, so the
/// old sockets can outlive `bind_all` by a few milliseconds.
pub fn bind_all_retry(
    cfg: &Config,
    retries: u32,
    delay: Duration,
) -> io::Result<Vec<TcpListener>> {
    let mut attempt = 0;
    loop {
        match bind_all_once(cfg) {
            Err(e) if attempt < retries && is_addr_in_use(&e) => {
                attempt += 1;
                std::thread::sleep(delay);
            }
            other => return other,
        }
    }
}

fn is_addr_in_use(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::AddrInUse || e.raw_os_error() == Some(10048)
}

fn bind_all_once(cfg: &Config) -> io::Result<Vec<TcpListener>> {
    let mut out = Vec::new();
    for spec in &cfg.listen {
        let (addr, dual) = split_listen(spec);
        let listener = bind_one(&addr)?;
        out.push(listener);
        // On Windows an IPv6 socket is v6-only by default, so `Dual` needs a
        // second socket for the IPv4 wildcard. Unix reaches both families
        // through the same `[::]` socket.
        if dual && cfg!(windows) {
            if let Ok(port) = port_of(&addr) {
                if let Ok(v4) = bind_one(&format!("0.0.0.0:{}", port)) {
                    out.push(v4);
                }
            }
        }
    }
    if out.is_empty() {
        out.push(bind_one(&crate::config::DEFAULT_PORT.to_string())?);
    }
    Ok(out)
}

/// Splits `[::]:4545,Dual` into the address and the `Dual` flag.
fn split_listen(spec: &str) -> (String, bool) {
    let mut parts = spec.splitn(2, ',');
    let addr = parts.next().unwrap_or("").trim().to_string();
    let dual = parts
        .next()
        .map(|opt| opt.trim().eq_ignore_ascii_case("dual"))
        .unwrap_or(false);
    (addr, dual)
}

fn port_of(addr: &str) -> io::Result<u16> {
    addr.to_socket_addrs()?
        .next()
        .map(|a| a.port())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no address"))
}

fn bind_one(spec: &str) -> io::Result<TcpListener> {
    let text = spec.trim();
    let target = if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) {
        format!("0.0.0.0:{}", text)
    } else {
        text.to_string()
    };
    let addrs: Vec<SocketAddr> = target
        .to_socket_addrs()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("{}: {}", spec, e)))?
        .collect();
    let mut last_err = None;
    for addr in addrs {
        match TcpListener::bind(addr) {
            Ok(l) => {
                disown(&l);
                return Ok(l);
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot resolve listen address: {}", spec),
        )
    }))
}

/// Clears the inherit flag on a listening socket.
///
/// A tray restart spawns the replacement process while the old one is still
/// alive. `TcpListener::bind` already yields a non-inheritable handle, but
/// `try_clone` (used by `Server::run` to hand the listener to an accept thread)
/// duplicates it as inheritable. The child then receives a copy, and its own
/// `bind` fails with `WSAEADDRINUSE` (os error 10048) because the leaked handle
/// keeps the port reserved until the child exits.
#[cfg(windows)]
fn disown(listener: &TcpListener) {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawSocket;

    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;

    #[link(name = "kernel32")]
    extern "system" {
        fn SetHandleInformation(handle: *mut c_void, mask: u32, flags: u32) -> i32;
    }

    unsafe {
        SetHandleInformation(listener.as_raw_socket() as *mut c_void, HANDLE_FLAG_INHERIT, 0);
    }
}

/// No handle inheritance on this platform.
#[cfg(not(windows))]
fn disown(_listener: &TcpListener) {}

impl Server {
    pub fn new(cfg: Arc<Config>, listeners: Vec<TcpListener>) -> Self {
        Server {
            cfg,
            listeners,
            sessions: Sessions::new(),
            active: AtomicUsize::new(0),
            shutdown: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn run(self: Arc<Self>) -> io::Result<()> {
        let mut handles = Vec::new();
        for listener in self.listeners.iter() {
            let listener = listener.try_clone()?;
            // `try_clone` produces a fresh inheritable handle; clear it so a
            // restarting child never inherits the port reservation.
            disown(&listener);
            let me = self.clone();
            handles.push(std::thread::spawn(move || me.accept_loop(listener)));
        }
        for handle in handles {
            let _ = handle.join();
        }
        Ok(())
    }

    fn accept_loop(self: Arc<Self>, listener: TcpListener) {
        loop {
            let (stream, peer) = match listener.accept() {
                Ok(v) => v,
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
            };
            if self.shutdown.load(Ordering::Relaxed) != 0 {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                continue;
            }
            if self.active.load(Ordering::Relaxed) >= self.cfg.max_conns {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                continue;
            }
            self.active.fetch_add(1, Ordering::Relaxed);
            let me = self.clone();
            let spawned = std::thread::Builder::new()
                .stack_size(self.cfg.thread_stack)
                .spawn(move || {
                    if let Err(e) = me.handle_conn(stream, peer) {
                        crate::log::error(&format!("connection {}: {}", peer, e));
                    }
                    me.active.fetch_sub(1, Ordering::Relaxed);
                });
            if spawned.is_err() {
                self.active.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    fn handle_conn(&self, mut stream: TcpStream, peer: SocketAddr) -> io::Result<()> {
        stream.set_nodelay(true).ok();
        stream.set_read_timeout(Some(Duration::from_secs(120)))?;
        stream.set_write_timeout(Some(Duration::from_secs(120)))?;
        let read_half = stream.try_clone()?;
        let mut reader = BufReader::with_capacity(16 * 1024, read_half);
        let mut served = 0u32;

        loop {
            let req = match http::read_head(&mut reader) {
                Ok(Some(r)) => r,
                Ok(None) => return Ok(()),
                Err(e) => {
                    let resp = Response::error(400, "bad_request", &e.to_string());
                    let _ = http::write_response(&mut stream, "GET", resp, false);
                    return Ok(());
                }
            };

            if let Some(expect) = req.header("expect") {
                if expect.eq_ignore_ascii_case("100-continue") {
                    if http::write_continue(&mut stream).is_err() {
                        return Ok(());
                    }
                }
            }

            let token = req.cookie(auth::SESSION_COOKIE);
            let identity = auth::identify(&self.cfg, &self.sessions, token);
            // Bodies are not drained here, so only body-less requests may reuse
            // the connection; uploads keep the one-shot behaviour of before.
            let keep_alive = served + 1 < MAX_KEEPALIVE_REQUESTS
                && req.wants_keep_alive()
                && !matches!(req.method.as_str(), "POST" | "PUT")
                && req.content_length().unwrap_or(0) == 0;
            let remote = peer.ip().to_string();
            let resp = self.dispatch(&req, &identity, &remote, &mut reader);
            let status = resp.status;
            let result = http::write_response(&mut stream, &req.method, resp, keep_alive);
            crate::log::info(&format!(
                "{} {} {} -> {}",
                peer, req.method, req.target, status
            ));
            if let Err(e) = result {
                return Err(e);
            }
            if !keep_alive {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return Ok(());
            }
            served += 1;
        }
    }

    fn dispatch<S: io::Read>(
        &self,
        req: &Request,
        id: &Identity,
        remote: &str,
        reader: &mut S,
    ) -> Response {
        match req.method.as_str() {
            "GET" | "HEAD" | "POST" | "PUT" | "DELETE" | "OPTIONS" => {}
            _ => return Response::error(405, "method_not_allowed", "unsupported method"),
        }
        if req.method == "OPTIONS" {
            return Response::new(204).header("Allow", "GET, HEAD, POST, PUT, DELETE, OPTIONS");
        }

        if req.path == "/" || req.path == "/index.html" {
            if req.method != "GET" && req.method != "HEAD" {
                return Response::error(405, "method_not_allowed", "use GET");
            }
            return serve_asset(&assets::INDEX, req, true);
        }
        if req.path.starts_with("/assets/") {
            if req.method != "GET" && req.method != "HEAD" {
                return Response::error(405, "method_not_allowed", "use GET");
            }
            return match assets::find(&req.path) {
                Some(asset) => serve_asset(asset, req, false),
                None => Response::error(404, "not_found", "asset not found"),
            };
        }
        if req.path.starts_with("/_api/") {
            return self.api_route(req, id, remote, reader);
        }
        if req.path.starts_with("/files/") {
            if req.method != "GET" && req.method != "HEAD" {
                return Response::error(405, "method_not_allowed", "use GET");
            }
            return self.serve_file(req, id, &req.path["/files".len()..]);
        }
        if req.method != "GET" && req.method != "HEAD" {
            return Response::error(405, "method_not_allowed", "use GET");
        }
        self.serve_file(req, id, &req.path)
    }

    fn api_route<S: io::Read>(
        &self,
        req: &Request,
        id: &Identity,
        remote: &str,
        reader: &mut S,
    ) -> Response {
        let route = &req.path["/_api".len()..];
        match (route, req.method.as_str()) {
            ("/session", "GET") | ("/session", "HEAD") => api::session_info(&self.cfg, id),
            ("/login", "POST") => {
                let mut buf = Vec::new();
                let limit = req.content_length().unwrap_or(0).min(64 * 1024);
                let mut taken = std::io::Read::take(&mut *reader, limit);
                let _ = io::copy(&mut taken, &mut buf);
                let body = String::from_utf8_lossy(&buf).into_owned();
                api::login(&self.cfg, &self.sessions, req, &body, remote)
            }
            ("/logout", "POST") => api::logout(&self.sessions, id),
            ("/list", "GET") | ("/list", "HEAD") => api::list(&self.cfg, id, req),
            ("/mkdir", "POST") => api::mkdir(&self.cfg, id, req, reader),
            ("/newfile", "POST") => api::new_file(&self.cfg, id, req, reader),
            ("/rename", "POST") => api::rename(&self.cfg, id, req, reader),
            ("/copy", "POST") => api::copy(&self.cfg, id, req, reader),
            ("/delete", "POST") => api::delete(&self.cfg, id, req, reader),
            ("/upload", "PUT") | ("/upload", "POST") => api::upload(&self.cfg, id, req, reader),
            ("/upload", "GET") | ("/upload", "HEAD") => api::upload_probe(&self.cfg, id, req),
            ("/upload", "DELETE") => api::upload_cancel(&self.cfg, id, req),
            ("/read", "GET") | ("/read", "HEAD") => api::read_text(&self.cfg, id, req),
            ("/write", "PUT") | ("/write", "POST") => api::write_text(&self.cfg, id, req, reader),
            ("/trash", "GET") | ("/trash", "HEAD") => api::trash_list(&self.cfg, id, req),
            ("/trash/restore", "POST") => api::trash_restore(&self.cfg, id, req, reader),
            ("/trash/purge", "POST") => api::trash_purge(&self.cfg, id, req, reader),
            ("/trash/empty", "POST") => api::trash_empty(&self.cfg, id, req, reader),
            _ => Response::error(404, "no_such_endpoint", "unknown API endpoint"),
        }
    }

    fn serve_file(&self, req: &Request, id: &Identity, rel: &str) -> Response {
        if !id.can(PERM_READ) {
            return Response::error(403, "forbidden", "reading files is not permitted");
        }
        let path = match fsapi::resolve(&self.cfg.root, rel) {
            Ok(p) => p,
            Err(e) => return Response::error(e.status(), e.code(), &e.message()),
        };
        if let Err(e) = fsapi::ensure_inside(&self.cfg.root, &path) {
            return Response::error(e.status(), e.code(), &e.message());
        }
        if path.starts_with(crate::trash::dir(&self.cfg.root)) {
            return Response::error(404, "not_found", "file not found");
        }
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => return Response::error(404, "not_found", "file not found"),
        };
        if meta.is_dir() {
            // A directory is only downloadable as an archive; a plain browser
            // navigation still gets bounced to the UI so the folder opens there.
            if req.q("zip").is_none() {
                let target = format!(
                    "/#{}",
                    util::percent_encode_path(&fsapi::relative(&self.cfg.root, &path))
                );
                return Response::new(302)
                    .header("Location", target)
                    .header("Content-Length", "0");
            }
            return build_zip_response(&path);
        }
        build_file_response(req, &path, meta.len(), modified_secs(&meta))
    }
}

fn modified_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn serve_asset(asset: &'static assets::Asset, req: &Request, is_index: bool) -> Response {
    let etag = assets::etag_for(asset);
    if let Some(inm) = req.header("if-none-match") {
        if etag_matches(inm, &etag) {
            return Response::new(304).header("ETag", etag);
        }
    }
    let mut resp = Response::new(200)
        .header("Content-Type", asset.content_type)
        .header("ETag", etag)
        // Asset URLs are stable, so a long `immutable` lifetime would hide
        // UI updates from returning browsers. Revalidate with the ETag instead.
        .header("Cache-Control", "no-cache")
        .header("X-Frame-Options", "DENY");
    if is_index {
        resp = resp.header("Content-Security-Policy", CSP);
    }
    resp.with_body(asset.bytes().to_vec())
}

fn etag_matches(header: &str, etag: &str) -> bool {
    for candidate in header.split(',') {
        let c = candidate.trim().trim_start_matches("W/").trim_matches('"');
        if c == "*" || c == etag {
            return true;
        }
    }
    false
}

fn build_file_response(req: &Request, path: &Path, size: u64, mtime: u64) -> Response {
    let etag = util::file_etag(size, mtime);
    if let Some(inm) = req.header("if-none-match") {
        if etag_matches(inm, &etag) {
            return Response::new(304)
                .header("ETag", etag)
                .header("Last-Modified", util::http_date(mtime));
        }
    }

    let content_type = mime::for_path(path);
    let range_header = req.header("range");
    let spec = range::parse(range_header, size);
    if let RangeSpec::Unsatisfiable = spec {
        return Response::error(416, "range_not_satisfiable", "requested range not available")
            .header("Content-Range", format!("bytes */{}", size))
            .header("Accept-Ranges", "bytes");
    }

    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Response::error(404, "not_found", "file not found"),
    };

    let download = req.q("download").is_some();
    let mut resp = Response::new(200);
    let (offset, len, status) = match spec {
        RangeSpec::Partial { start, end } => {
            let len = end - start + 1;
            resp = resp.header("Content-Range", format!("bytes {}-{}/{}", start, end, size));
            (start, len, 206)
        }
        _ => (0, size, 200),
    };
    resp.status = status;
    resp = resp
        .header("Content-Type", content_type.clone())
        .header("Accept-Ranges", "bytes")
        .header("ETag", etag)
        .header("Last-Modified", util::http_date(mtime))
        .header("Cache-Control", "public, max-age=3600");
    if download || !mime::is_previewable(&content_type) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        // `inline` keeps previewable files rendering in the tab; `attachment`
        // is what a "download" click needs in order to save the bytes.
        let kind = if download { "attachment" } else { "inline" };
        resp = resp.header(
            "Content-Disposition",
            format!("{}; filename*=UTF-8''{}", kind, percent_encode(&name)),
        );
    }
    resp.body = Body::File {
        file,
        offset,
        len,
    };
    resp
}

fn build_zip_response(dir: &Path) -> Response {
    let zip = match ZipStream::new(dir) {
        Ok(z) => z,
        Err(zip::ZipError::TooLarge) => {
            return Response::error(413, "too_large", "folder is too large to archive")
        }
        Err(zip::ZipError::TooManyEntries) => {
            return Response::error(413, "too_many_entries", "folder has too many entries")
        }
        Err(zip::ZipError::Io(_)) => {
            return Response::error(500, "zip_failed", "folder could not be read")
        }
    };
    let len = zip.len();
    let name = dir
        .file_name()
        .map(|n| format!("{}.zip", n.to_string_lossy()))
        .unwrap_or_else(|| "download.zip".into());
    Response::new(200)
        .header("Content-Type", "application/zip")
        .header(
            "Content-Disposition",
            format!("attachment; filename*=UTF-8''{}", percent_encode(&name)),
        )
        .header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff")
        .with_stream(Box::new(zip), len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_listen_options() {
        assert_eq!(split_listen("[::]:4545,Dual"), ("[::]:4545".into(), true));
        assert_eq!(split_listen("4545"), ("4545".into(), false));
        assert_eq!(split_listen("127.0.0.1:80,dual"), ("127.0.0.1:80".into(), true));
    }

    #[test]
    fn detects_address_in_use() {
        assert!(is_addr_in_use(&io::Error::from_raw_os_error(10048)));
        assert!(is_addr_in_use(&io::Error::new(
            io::ErrorKind::AddrInUse,
            "busy"
        )));
        assert!(!is_addr_in_use(&io::Error::new(
            io::ErrorKind::PermissionDenied,
            "denied"
        )));
    }

    #[cfg(windows)]
    #[test]
    fn disown_clears_the_inherit_flag() {
        use std::ffi::c_void;
        use std::os::windows::io::AsRawSocket;

        const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;

        #[link(name = "kernel32")]
        extern "system" {
            fn GetHandleInformation(handle: *mut c_void, flags: *mut u32) -> i32;
        }

        fn inherit_flag(handle: *mut c_void) -> u32 {
            let mut flags = 0u32;
            assert_eq!(unsafe { GetHandleInformation(handle, &mut flags) }, 1);
            flags & HANDLE_FLAG_INHERIT
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        // `bind` hands out a non-inheritable handle, but `try_clone` (used by
        // `Server::run` for the accept threads) duplicates it as inheritable,
        // which is what leaked the port reservation into a restarted child.
        let clone = listener.try_clone().unwrap();
        let clone_handle = clone.as_raw_socket() as *mut c_void;
        assert_ne!(
            inherit_flag(clone_handle),
            0,
            "try_clone duplicates the handle as inheritable"
        );

        disown(&clone);

        assert_eq!(inherit_flag(clone_handle), 0);
        assert_eq!(
            inherit_flag(listener.as_raw_socket() as *mut c_void),
            0,
            "the original handle stays non-inheritable"
        );
    }

    #[test]
    fn matches_weak_and_list_etags() {
        assert!(etag_matches("W/\"abc\"", "abc"));
        assert!(etag_matches("\"x\", \"abc\"", "abc"));
        assert!(etag_matches("*", "abc"));
        assert!(!etag_matches("\"zzz\"", "abc"));
    }
}
