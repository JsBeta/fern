//! End-to-end test: boots the real binary on a free port and drives it over TCP.
//!
//! Everything the bundled UI relies on is exercised here: static assets, the
//! JSON API, chunked/resumable uploads, range requests, permissions and the
//! path traversal guards.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Harness {
    child: Child,
    root: PathBuf,
    port: u16,
    conf_dir: Option<PathBuf>,
}

impl Harness {
    fn start() -> Harness {
        Harness::start_env(&[])
    }

    fn start_env(env: &[(&str, &str)]) -> Harness {
        Harness::start_conf_env(env, None)
    }

    /// Boots the binary with `fern.conf` in the working directory. `@ROOT@` and
    /// `@PORT@` are replaced with the harness values.
    fn start_conf(conf: &str) -> Harness {
        Harness::start_conf_env(&[], Some(conf))
    }

    fn start_conf_env(env: &[(&str, &str)], conf: Option<&str>) -> Harness {
        let port = free_port();
        let stamp = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let root = std::env::temp_dir().join(format!("fern-it-{}", stamp));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).expect("create root");
        std::fs::write(root.join("hello.txt"), b"hello world").expect("seed file");
        std::fs::write(root.join("sub").join("nested.txt"), b"nested").expect("seed nested");

        // The config is looked up in the working directory, so give it one of
        // its own to keep the shared root free of test files.
        let mut conf_dir = None;
        if let Some(text) = conf {
            let dir = std::env::temp_dir().join(format!("fern-conf-{}", stamp));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create conf dir");
            let text = text
                .replace("@ROOT@", root.to_str().unwrap())
                .replace("@PORT@", &port.to_string());
            std::fs::write(dir.join("fern.conf"), text).expect("write fern.conf");
            conf_dir = Some(dir);
        }

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fern"));
        cmd.args([
            "-w",
            root.to_str().unwrap(),
            "-l",
            &format!("127.0.0.1:{}", port),
            "-B",
            "-h",
            "-g",
            "r",
            "-u",
            "admin:pw:RLUMST",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        if let Some(dir) = &conf_dir {
            cmd.current_dir(dir);
        }
        for (key, value) in env {
            cmd.env(key, value);
        }
        let child = cmd.spawn().expect("spawn fern");

        let h = Harness {
            child,
            root,
            port,
            conf_dir,
        };
        h.wait_ready();
        h
    }

    fn wait_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("server did not start listening on port {}", self.port);
    }

    fn request(&self, method: &str, path: &str, body: &[u8]) -> HttpResponse {
        self.request_with(method, path, body, &[])
    }

    fn request_with(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        extra: &[(&str, &str)],
    ) -> HttpResponse {
        let mut raw = format!(
            "{} {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: {}\r\n",
            method,
            path,
            body.len()
        );
        for (k, v) in extra {
            raw.push_str(&format!("{}: {}\r\n", k, v));
        }
        raw.push_str("\r\n");
        let mut bytes = raw.into_bytes();
        bytes.extend_from_slice(body);
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream.write_all(&bytes).expect("write request");
        stream.flush().unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).expect("read response");
        HttpResponse::parse(&buf)
    }

    fn login(&self) -> String {
        let resp = self.request(
            "POST",
            "/_api/login",
            br#"{"user":"admin","pass":"pw"}"#,
        );
        assert_eq!(resp.status, 200, "login failed: {}", resp.text());
        let cookie = resp
            .header("set-cookie")
            .expect("login must set a cookie")
            .split(';')
            .next()
            .unwrap()
            .to_string();
        cookie
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
        if let Some(dir) = &self.conf_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn parse(buf: &[u8]) -> HttpResponse {
        let split = find(buf, b"\r\n\r\n").expect("response must contain a header block");
        let head = String::from_utf8_lossy(&buf[..split]).into_owned();
        let body = buf[split + 4..].to_vec();
        let mut lines = head.split("\r\n");
        let status_line = lines.next().unwrap_or_default();
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let headers = lines
            .filter_map(|l| {
                let (k, v) = l.split_once(':')?;
                Some((k.trim().to_ascii_lowercase(), v.trim().to_string()))
            })
            .collect();
        HttpResponse {
            status,
            headers,
            body,
        }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn free_port() -> u16 {
    // An ephemeral port is only reserved until the listener is dropped, so
    // parallel tests can be handed the same number; remember what was handed
    // out and draw again instead of letting two servers fight over a port.
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static TAKEN: OnceLock<Mutex<HashSet<u16>>> = OnceLock::new();
    let taken = TAKEN.get_or_init(|| Mutex::new(HashSet::new()));
    loop {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = l.local_addr().unwrap().port();
        drop(l);
        if taken.lock().unwrap().insert(port) {
            return port;
        }
    }
}

fn contains(hay: &str, needle: &str) -> bool {
    hay.contains(needle)
}

/// Pulls a numeric field out of a JSON response body.
fn json_num(text: &str, key: &str) -> f64 {
    let pat = format!("\"{}\":", key);
    let start = text.find(&pat).expect("field present") + pat.len();
    let rest = &text[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .unwrap_or(rest.len());
    rest[..end].parse().expect("field is a number")
}

#[test]
fn serves_ui_and_assets() {
    let h = Harness::start();
    let resp = h.request("GET", "/", b"");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.header("content-type"), Some("text/html; charset=utf-8"));
    assert!(contains(&resp.text(), "<title>fern</title>"));
    assert!(resp.header("content-security-policy").is_some());

    let resp = h.request("GET", "/assets/app.js", b"");
    assert_eq!(resp.status, 200);
    assert_eq!(
        resp.header("content-type"),
        Some("text/javascript; charset=utf-8")
    );
    let etag = resp.header("etag").unwrap().to_string();
    assert!(resp.text().len() > 1000);

    let resp = h.request_with("GET", "/assets/app.js", b"", &[("If-None-Match", &etag)]);
    assert_eq!(resp.status, 304);

    let resp = h.request("GET", "/assets/missing.js", b"");
    assert_eq!(resp.status, 404);
}

#[test]
fn guest_permissions_are_enforced() {
    let h = Harness::start();
    let resp = h.request("GET", "/_api/session", b"");
    assert_eq!(resp.status, 200);
    assert!(contains(&resp.text(), "\"guest\":true"));
    assert!(contains(&resp.text(), "\"letters\":\"R\""));

    // Guests may read files but not list directories.
    assert_eq!(h.request("GET", "/files/hello.txt", b"").status, 200);
    assert_eq!(h.request("GET", "/_api/list?path=/", b"").status, 403);
    assert_eq!(
        h.request("PUT", "/_api/upload?path=/&name=x.txt", b"data")
            .status,
        403
    );
}

#[test]
fn range_and_conditional_downloads() {
    let h = Harness::start();
    let resp = h.request("GET", "/files/hello.txt", b"");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.text(), "hello world");
    assert_eq!(resp.header("accept-ranges"), Some("bytes"));
    let etag = resp.header("etag").unwrap().to_string();

    let resp = h.request_with(
        "GET",
        "/files/hello.txt",
        b"",
        &[("Range", "bytes=6-10")],
    );
    assert_eq!(resp.status, 206);
    assert_eq!(resp.text(), "world");
    assert_eq!(resp.header("content-range"), Some("bytes 6-10/11"));

    let resp = h.request_with(
        "GET",
        "/files/hello.txt",
        b"",
        &[("Range", "bytes=100-200")],
    );
    assert_eq!(resp.status, 416);

    let resp = h.request_with("GET", "/files/hello.txt", b"", &[("If-None-Match", &etag)]);
    assert_eq!(resp.status, 304);

    let resp = h.request("GET", "/files/sub", b"");
    assert_eq!(resp.status, 302);
    assert_eq!(resp.header("location"), Some("/#/sub"));
}

#[test]
fn rejects_path_traversal() {
    let h = Harness::start();
    let cookie = h.login();
    let auth = [("Cookie", cookie.as_str())];
    for path in [
        "/files/../fern.conf",
        "/files/..%2f..%2fwindows%2fwin.ini",
        "/files/..%5c..%5cwindows%5cwin.ini",
        "/_api/list?path=..%2f..",
    ] {
        let resp = h.request_with("GET", path, b"", &auth);
        assert!(
            resp.status == 400 || resp.status == 404,
            "{} returned {}",
            path,
            resp.status
        );
    }
}

/// Directory names currently inside `<root>/.Trash`, i.e. the entry ids.
fn trash_ids(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(root.join(".Trash")) else {
        return out;
    };
    for item in read_dir.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if !name.starts_with('.') {
            out.push(name);
        }
    }
    out.sort();
    out
}

#[test]
fn authenticated_file_management() {
    let h = Harness::start();
    let cookie = h.login();
    let auth = [("Cookie", cookie.as_str())];

    let resp = h.request_with("GET", "/_api/session", b"", &auth);
    assert!(contains(&resp.text(), "\"user\":\"admin\""));

    let resp = h.request_with("GET", "/_api/list?path=/", b"", &auth);
    assert_eq!(resp.status, 200);
    assert!(contains(&resp.text(), "\"name\":\"hello.txt\""));
    assert!(contains(&resp.text(), "\"isDir\":true"));

    let resp = h.request_with(
        "POST",
        "/_api/mkdir",
        br#"{"path":"/","name":"docs"}"#,
        &auth,
    );
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(h.root.join("docs").is_dir());

    let resp = h.request_with(
        "POST",
        "/_api/rename",
        br#"{"from":"hello.txt","to":"docs/hello.txt"}"#,
        &auth,
    );
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(h.root.join("docs").join("hello.txt").is_file());

    let resp = h.request_with(
        "POST",
        "/_api/delete",
        br#"{"paths":["docs/hello.txt"]}"#,
        &auth,
    );
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(contains(&resp.text(), "\"moved\":1"));
    assert!(!h.root.join("docs").join("hello.txt").exists());
    let ids = trash_ids(&h.root);
    assert_eq!(ids.len(), 1, "delete must create exactly one trash entry");
    let entry = h.root.join(".Trash").join(&ids[0]);
    assert!(entry.join("meta.json").is_file());
    assert!(entry.join("data").join("hello.txt").is_file());
    let meta = std::fs::read_to_string(entry.join("meta.json")).unwrap();
    assert!(contains(&meta, "\"origPath\":\"/docs/hello.txt\""), "{}", meta);
    assert!(contains(&meta, "\"deletedBy\":\"admin\""), "{}", meta);
}

#[test]
fn copy_duplicates_files_and_folders() {
    let h = Harness::start();
    let cookie = h.login();
    let auth = [("Cookie", cookie.as_str())];

    // A directory copy recreates the whole tree under a suffixed name.
    let resp = h.request_with("POST", "/_api/copy", br#"{"paths":["sub"],"dest":"/"}"#, &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(contains(&resp.text(), "\"copied\":1"));
    let copy = h.root.join("sub - 副本");
    assert!(copy.join("nested.txt").is_file(), "directory copy must recurse");
    assert!(h.root.join("sub").join("nested.txt").is_file(), "source must survive");

    // Pasting twice into the same directory never overwrites.
    let resp = h.request_with("POST", "/_api/copy", br#"{"paths":["sub"],"dest":"/"}"#, &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(h.root.join("sub - 副本 (2)").join("nested.txt").is_file());

    // Files keep their extension and can be copied into another directory.
    std::fs::create_dir_all(h.root.join("target")).unwrap();
    let resp = h.request_with(
        "POST",
        "/_api/copy",
        br#"{"paths":["hello.txt"],"dest":"/target"}"#,
        &auth,
    );
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(h.root.join("target").join("hello - 副本.txt").is_file());

    let resp = h.request_with("POST", "/_api/copy", br#"{"paths":["missing"],"dest":"/"}"#, &auth);
    assert_eq!(resp.status, 404, "{}", resp.text());

    let resp = h.request_with("POST", "/_api/copy", br#"{"paths":[],"dest":"/"}"#, &auth);
    assert_eq!(resp.status, 400, "{}", resp.text());
}

#[test]
fn session_exposes_title_and_notice() {
    let h = Harness::start();
    let resp = h.request("GET", "/_api/session", b"");
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(contains(&resp.text(), "\"title\":\"fern\""), "{}", resp.text());
    assert!(contains(&resp.text(), "\"notice\":\"\""), "{}", resp.text());
}

#[test]
fn conf_sets_title_and_notice() {
    let h = Harness::start_conf(
        "Fern.Title \"团队 网盘\"\nFern.Notice \"今晚 22:00 停机维护\"\n",
    );
    let resp = h.request("GET", "/_api/session", b"");
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(contains(&resp.text(), "\"title\":\"团队 网盘\""), "{}", resp.text());
    assert!(
        contains(&resp.text(), "\"notice\":\"今晚 22:00 停机维护\""),
        "{}",
        resp.text()
    );
}

#[test]
fn new_file_creates_empty_file() {
    let h = Harness::start();
    let cookie = h.login();
    let auth = [("Cookie", cookie.as_str())];

    std::fs::create_dir_all(h.root.join("docs")).unwrap();
    let resp = h.request_with(
        "POST",
        "/_api/newfile",
        br#"{"path":"/docs","name":"notes.md"}"#,
        &auth,
    );
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(contains(&resp.text(), "\"path\":\"/docs/notes.md\""), "{}", resp.text());
    let created = h.root.join("docs").join("notes.md");
    assert!(created.is_file());
    assert_eq!(std::fs::metadata(&created).unwrap().len(), 0);

    let resp = h.request_with("GET", "/_api/list?path=/docs", b"", &auth);
    assert!(contains(&resp.text(), "\"name\":\"notes.md\""));

    // The file can be filled in through the regular write endpoint afterwards.
    let resp = h.request_with(
        "PUT",
        "/_api/write",
        br#"{"path":"/docs/notes.md","content":"hi","mtimeMs":0,"bom":false,"force":true}"#,
        &auth,
    );
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(std::fs::read_to_string(&created).unwrap(), "hi");

    // Never overwrite an existing entry.
    let resp = h.request_with(
        "POST",
        "/_api/newfile",
        br#"{"path":"/docs","name":"notes.md"}"#,
        &auth,
    );
    assert_eq!(resp.status, 409, "{}", resp.text());
    assert_eq!(std::fs::read_to_string(&created).unwrap(), "hi");

    // Names with separators and reserved internal suffixes are rejected.
    let resp = h.request_with(
        "POST",
        "/_api/newfile",
        br#"{"path":"/docs","name":"a/b.txt"}"#,
        &auth,
    );
    assert_eq!(resp.status, 400, "{}", resp.text());
    let resp = h.request_with(
        "POST",
        "/_api/newfile",
        br#"{"path":"/","name":"../escape.txt"}"#,
        &auth,
    );
    assert_eq!(resp.status, 400, "{}", resp.text());
    assert!(!h.root.join("escape.txt").exists());
    let resp = h.request_with(
        "POST",
        "/_api/newfile",
        br#"{"path":"/","name":"x.fernpart"}"#,
        &auth,
    );
    assert_eq!(resp.status, 400, "{}", resp.text());
}

#[test]
fn new_file_requires_modify_permission() {
    let h = Harness::start();
    let resp = h.request(
        "POST",
        "/_api/newfile",
        br#"{"path":"/","name":"guest.txt"}"#,
    );
    assert_eq!(resp.status, 403, "{}", resp.text());
    assert!(!h.root.join("guest.txt").exists());
}

#[test]
fn trash_lists_restores_and_purges() {
    let h = Harness::start();
    let cookie = h.login();
    let auth = [("Cookie", cookie.as_str())];

    // The UI hides the recycle bin unless the session reports it as enabled.
    let resp = h.request_with("GET", "/_api/session", b"", &auth);
    assert!(contains(&resp.text(), "\"trashEnabled\":true"), "{}", resp.text());

    // The bin is only reachable through its own endpoint.
    let resp = h.request_with("POST", "/_api/delete", br#"{"paths":["hello.txt"]}"#, &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());

    let resp = h.request_with("GET", "/_api/list?path=/.Trash", b"", &auth);
    assert_eq!(resp.status, 403, "{}", resp.text());
    let ids = trash_ids(&h.root);
    let resp = h.request_with(
        "GET",
        &format!("/files/.Trash/{}/data/hello.txt", ids[0]),
        b"",
        &auth,
    );
    assert_eq!(resp.status, 404, "{}", resp.text());

    let resp = h.request_with("GET", "/_api/trash", b"", &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(json_num(&resp.text(), "total"), 1.0);
    assert!(contains(&resp.text(), "\"name\":\"hello.txt\""));
    assert!(contains(&resp.text(), "\"path\":\"/hello.txt\""));
    assert!(contains(&resp.text(), "\"deletedBy\":\"admin\""));

    let ids = trash_ids(&h.root);
    assert_eq!(ids.len(), 1);
    let body = format!(r#"{{"ids":["{}"]}}"#, ids[0]);
    let resp = h.request_with("POST", "/_api/trash/restore", body.as_bytes(), &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(json_num(&resp.text(), "restored"), 1.0);
    assert_eq!(
        std::fs::read(h.root.join("hello.txt")).unwrap(),
        b"hello world"
    );
    assert!(trash_ids(&h.root).is_empty());

    // Restoring over an existing file needs an explicit overwrite and the
    // displaced file goes back into the bin instead of being destroyed.
    let resp = h.request_with("POST", "/_api/delete", br#"{"paths":["hello.txt"]}"#, &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    std::fs::write(h.root.join("hello.txt"), b"newer").unwrap();
    let ids = trash_ids(&h.root);
    let body = format!(r#"{{"ids":["{}"]}}"#, ids[0]);
    let resp = h.request_with("POST", "/_api/trash/restore", body.as_bytes(), &auth);
    assert_eq!(resp.status, 409, "{}", resp.text());
    assert!(contains(&resp.text(), "\"code\":\"exists\""));

    let body = format!(r#"{{"ids":["{}"],"overwrite":true}}"#, ids[0]);
    let resp = h.request_with("POST", "/_api/trash/restore", body.as_bytes(), &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(
        std::fs::read(h.root.join("hello.txt")).unwrap(),
        b"hello world"
    );
    assert_eq!(trash_ids(&h.root).len(), 1, "the replaced file is kept");

    // Purge drops a single entry, empty drops them all.
    let ids = format!(r#"{{"ids":["{}"]}}"#, trash_ids(&h.root)[0]);
    let resp = h.request_with("POST", "/_api/trash/purge", ids.as_bytes(), &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(json_num(&resp.text(), "purged"), 1.0);
    assert!(trash_ids(&h.root).is_empty());

    for path in ["a.txt", "b.txt"] {
        std::fs::write(h.root.join(path), b"x").unwrap();
        let resp = h.request_with(
            "POST",
            "/_api/delete",
            format!(r#"{{"paths":["{}"]}}"#, path).as_bytes(),
            &auth,
        );
        assert_eq!(resp.status, 200, "{}", resp.text());
    }
    assert_eq!(trash_ids(&h.root).len(), 2);
    let resp = h.request_with("POST", "/_api/trash/empty", b"{}", &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(json_num(&resp.text(), "purged"), 2.0);
    assert!(trash_ids(&h.root).is_empty());
}

#[test]
fn trash_disabled_deletes_immediately() {
    let h = Harness::start_env(&[("_TRASH_ENABLED", "0")]);
    let cookie = h.login();
    let auth = [("Cookie", cookie.as_str())];

    let resp = h.request_with("GET", "/_api/session", b"", &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(contains(&resp.text(), "\"trashEnabled\":false"));

    let resp = h.request_with("POST", "/_api/delete", br#"{"paths":["hello.txt"]}"#, &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(json_num(&resp.text(), "moved"), 1.0);
    assert!(contains(&resp.text(), "\"permanent\":true"));
    assert!(!h.root.join("hello.txt").exists(), "file is gone for good");
    assert!(
        !h.root.join(".Trash").exists(),
        "the bin is never created when disabled"
    );
}

#[test]
fn trash_requires_modify_permission() {
    let h = Harness::start();
    let resp = h.request("GET", "/_api/trash", b"");
    assert_eq!(resp.status, 403, "{}", resp.text());
    let resp = h.request("POST", "/_api/trash/restore", br#"{"ids":["x"]}"#);
    assert_eq!(resp.status, 403, "{}", resp.text());
    let resp = h.request("POST", "/_api/trash/empty", b"{}");
    assert_eq!(resp.status, 403, "{}", resp.text());
}

#[test]
fn copy_requires_modify_permission() {
    let h = Harness::start();
    let resp = h.request("POST", "/_api/copy", br#"{"paths":["hello.txt"],"dest":"/"}"#);
    assert_eq!(resp.status, 403, "{}", resp.text());
    assert!(!h.root.join("hello - 副本.txt").exists());
}

#[test]
fn resumable_upload() {
    let h = Harness::start();
    let cookie = h.login();
    let auth = [("Cookie", cookie.as_str())];

    let resp = h.request_with(
        "PUT",
        "/_api/upload?path=/&name=up.txt&offset=0&total=10",
        b"ABCDE",
        &auth,
    );
    assert_eq!(resp.status, 200, "{}", resp.text());

    let resp = h.request_with(
        "PUT",
        "/_api/upload?path=/&name=up.txt&offset=5&total=10",
        b"FGHIJ",
        &auth,
    );
    assert_eq!(resp.status, 200, "{}", resp.text());

    let resp = h.request("GET", "/files/up.txt", b"");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.text(), "ABCDEFGHIJ");

    // A stale offset must be reported together with the real one. Once the
    // upload finished there is no partial file left, so the server reports 0
    // and the client restarts from the beginning.
    let resp = h.request_with(
        "PUT",
        "/_api/upload?path=/&name=up.txt&offset=3&total=20",
        b"XYZ",
        &auth,
    );
    assert_eq!(resp.status, 409);
    assert!(contains(&resp.text(), "\"error\":\"offset_mismatch\""));
    assert!(contains(&resp.text(), "\"offset\":0"));
}

#[test]
fn upload_resume_keeps_partial_data() {
    let h = Harness::start();
    let cookie = h.login();
    let auth = [("Cookie", cookie.as_str())];

    let resp = h.request_with(
        "PUT",
        "/_api/upload?path=/&name=r.txt&offset=0&total=10",
        b"ABCDE",
        &auth,
    );
    assert_eq!(resp.status, 200, "{}", resp.text());

    // The UI probes first so a reloaded page knows where to pick up.
    let resp = h.request_with("GET", "/_api/upload?path=/&name=r.txt", b"", &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(contains(&resp.text(), "\"offset\":5"), "{}", resp.text());

    // Starting over at zero now reports the real offset instead of silently
    // discarding the bytes already received.
    let resp = h.request_with(
        "PUT",
        "/_api/upload?path=/&name=r.txt&offset=0&total=10",
        b"ZZZZZ",
        &auth,
    );
    assert_eq!(resp.status, 409, "{}", resp.text());
    assert!(contains(&resp.text(), "\"offset\":5"), "{}", resp.text());
    assert_eq!(h.root.join("r.txt.fernpart").metadata().unwrap().len(), 5);

    let resp = h.request_with(
        "PUT",
        "/_api/upload?path=/&name=r.txt&offset=5&total=10",
        b"FGHIJ",
        &auth,
    );
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(contains(&resp.text(), "\"done\":true"), "{}", resp.text());
    let resp = h.request("GET", "/files/r.txt", b"");
    assert_eq!(resp.text(), "ABCDEFGHIJ");

    // An explicit restart discards the leftovers on purpose.
    let resp = h.request_with(
        "PUT",
        "/_api/upload?path=/&name=r.txt&offset=0&total=10",
        b"abcde",
        &auth,
    );
    assert_eq!(resp.status, 200, "{}", resp.text());
    let resp = h.request_with(
        "PUT",
        "/_api/upload?path=/&name=r.txt&offset=0&total=10&restart=1",
        b"12345",
        &auth,
    );
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(contains(&resp.text(), "\"offset\":5"), "{}", resp.text());
}

#[test]
fn cancelled_upload_drops_partial_data() {
    let h = Harness::start();
    let cookie = h.login();
    let auth = [("Cookie", cookie.as_str())];

    let resp = h.request_with(
        "PUT",
        "/_api/upload?path=/&name=c.txt&offset=0&total=10",
        b"ABCDE",
        &auth,
    );
    assert_eq!(resp.status, 200, "{}", resp.text());
    let part = h.root.join("c.txt.fernpart");
    assert!(part.exists());

    // Partial data is server bookkeeping and must never look like a real file.
    let body = h.request_with("GET", "/_api/list?path=/", b"", &auth).text();
    let at = body.find("c.txt.fernpart").expect("part is listed for admins");
    let window = &body[at..(at + 200).min(body.len())];
    assert!(window.contains("\"hidden\":true"), "{}", window);

    let resp = h.request_with("DELETE", "/_api/upload?path=/&name=c.txt", b"", &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(!part.exists());
}

#[test]
fn text_edit_roundtrip_and_conflicts() {
    let h = Harness::start();
    let cookie = h.login();
    let auth = [("Cookie", cookie.as_str())];

    // Reading returns the content plus the metadata the editor needs.
    let resp = h.request_with("GET", "/_api/read?path=/hello.txt", b"", &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.text();
    assert!(contains(&body, "\"content\":\"hello world\""), "{}", body);
    assert!(contains(&body, "\"bom\":false"), "{}", body);
    assert!(contains(&body, "\"canEdit\":true"), "{}", body);
    let mtime = json_num(&body, "mtimeMs");
    assert!(mtime > 0.0);

    // A matching mtime saves the content in place.
    let save = format!(
        r#"{{"path":"/hello.txt","content":"line1\nline2","mtimeMs":{}}}"#,
        mtime
    );
    let resp = h.request_with("PUT", "/_api/write", save.as_bytes(), &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(json_num(&resp.text(), "size"), 11.0);
    assert_eq!(
        std::fs::read_to_string(h.root.join("hello.txt")).unwrap(),
        "line1\nline2"
    );

    // A stale mtime is rejected; `force` lets the client overwrite anyway.
    let stale = br#"{"path":"/hello.txt","content":"stale","mtimeMs":1}"#;
    let resp = h.request_with("PUT", "/_api/write", stale, &auth);
    assert_eq!(resp.status, 409, "{}", resp.text());
    assert!(contains(&resp.text(), "\"error\":\"conflict\""));
    assert_eq!(
        std::fs::read_to_string(h.root.join("hello.txt")).unwrap(),
        "line1\nline2"
    );

    let forced = br#"{"path":"/hello.txt","content":"forced","mtimeMs":1,"force":true}"#;
    let resp = h.request_with("PUT", "/_api/write", forced, &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(
        std::fs::read_to_string(h.root.join("hello.txt")).unwrap(),
        "forced"
    );

    // Binary files and directories are not editable text.
    std::fs::write(h.root.join("blob.bin"), [0u8, 1, 2, 3]).unwrap();
    let resp = h.request_with("GET", "/_api/read?path=/blob.bin", b"", &auth);
    assert_eq!(resp.status, 415, "{}", resp.text());
    assert!(contains(&resp.text(), "\"error\":\"not_text\""));

    let resp = h.request_with("GET", "/_api/read?path=/sub", b"", &auth);
    assert_eq!(resp.status, 400, "{}", resp.text());
    assert!(contains(&resp.text(), "\"error\":\"is_directory\""));

    // Guests may read text but never write it.
    let resp = h.request("GET", "/_api/read?path=/hello.txt", b"");
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(contains(&resp.text(), "\"canEdit\":false"));
    let resp = h.request(
        "PUT",
        "/_api/write",
        br#"{"path":"/hello.txt","content":"hacked"}"#,
    );
    assert_eq!(resp.status, 403, "{}", resp.text());
    assert_eq!(
        std::fs::read_to_string(h.root.join("hello.txt")).unwrap(),
        "forced"
    );
}

#[test]
fn edit_round_trips_a_byte_order_mark() {
    let h = Harness::start();
    let cookie = h.login();
    let auth = [("Cookie", cookie.as_str())];
    std::fs::write(h.root.join("bom.txt"), b"\xEF\xBB\xBFhello").unwrap();

    let resp = h.request_with("GET", "/_api/read?path=/bom.txt", b"", &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.text();
    assert!(contains(&body, "\"bom\":true"), "{}", body);
    assert!(contains(&body, "\"content\":\"hello\""), "{}", body);
    let mtime = json_num(&body, "mtimeMs");

    let save = format!(
        r#"{{"path":"/bom.txt","content":"salut","mtimeMs":{},"bom":true}}"#,
        mtime
    );
    let resp = h.request_with("PUT", "/_api/write", save.as_bytes(), &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(
        std::fs::read(h.root.join("bom.txt")).unwrap(),
        b"\xEF\xBB\xBFsalut"
    );
}

/// Folder uploads send `name` with separators relative to the browsed folder,
/// so the parent directory legitimately does not exist yet.
#[test]
fn upload_creates_missing_subdirectories() {
    let h = Harness::start();
    let cookie = h.login();
    let auth = [("Cookie", cookie.as_str())];

    let resp = h.request_with(
        "PUT",
        "/_api/upload?path=/&name=pkg/sub/deep/file.bin&offset=0&total=6",
        b"abcdef",
        &auth,
    );
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(contains(&resp.text(), "\"done\":true"), "{}", resp.text());
    assert_eq!(
        std::fs::read(h.root.join("pkg").join("sub").join("deep").join("file.bin")).unwrap(),
        b"abcdef"
    );

    // Chunked folder uploads resume the same way once the tree exists.
    let resp = h.request_with(
        "PUT",
        "/_api/upload?path=&name=pkg/next.bin&offset=0&total=3",
        b"xyz",
        &auth,
    );
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(
        std::fs::read(h.root.join("pkg").join("next.bin")).unwrap(),
        b"xyz"
    );
}

/// Early error replies used to skip the request body, so the server closed a
/// socket that still held data and Windows answered with RST; the browser then
/// reported `Failed to fetch` instead of the JSON error.
#[test]
fn upload_errors_drain_the_body_before_replying() {
    let h = Harness::start();
    let cookie = h.login();
    let auth = [("Cookie", cookie.as_str())];
    let body = vec![b'x'; 2 * 1024 * 1024];

    let resp = h.request_with(
        "PUT",
        "/_api/upload?path=/&name=..%5Cescape.bin&offset=0&total=2097152",
        &body,
        &auth,
    );
    assert_eq!(resp.status, 400, "{}", resp.text());
    assert!(contains(&resp.text(), "\"error\":\"bad_path\""), "{}", resp.text());

    let resp = h.request_with(
        "PUT",
        "/_api/upload?path=/&name=%00bad&offset=0&total=2097152",
        &body,
        &auth,
    );
    assert_eq!(resp.status, 400, "{}", resp.text());
    assert!(contains(&resp.text(), "\"error\":\"bad_name\""), "{}", resp.text());

    // A stale partial on disk makes the next full chunk a 409, which must also
    // arrive as JSON rather than as a reset connection.
    let resp = h.request_with(
        "PUT",
        "/_api/upload?path=/&name=drain.bin&offset=0&total=2097152",
        &body[..1024],
        &auth,
    );
    assert_eq!(resp.status, 200, "{}", resp.text());
    let resp = h.request_with(
        "PUT",
        "/_api/upload?path=/&name=drain.bin&offset=0&total=2097152",
        &body,
        &auth,
    );
    assert_eq!(resp.status, 409, "{}", resp.text());
    assert!(contains(&resp.text(), "\"offset\":1024"), "{}", resp.text());
}

/// A folder download has to be a real archive: it used to redirect to the UI,
/// so the browser saved the HTML page as `<folder>.htm`.
#[test]
fn folder_download_streams_a_zip_archive() {
    let h = Harness::start();
    let cookie = h.login();
    let auth = [("Cookie", cookie.as_str())];
    for (name, data) in [
        ("pkg/a.txt", b"hello".as_slice()),
        ("pkg/sub/b.bin", b"0123456789".as_slice()),
    ] {
        let resp = h.request_with(
            "PUT",
            &format!(
                "/_api/upload?path=/&name={}&offset=0&total={}",
                name,
                data.len()
            ),
            data,
            &auth,
        );
        assert_eq!(resp.status, 200, "{}", resp.text());
    }

    // Without the flag the plain browser navigation still opens the folder.
    let resp = h.request_with("GET", "/files/pkg", b"", &auth);
    assert_eq!(resp.status, 302, "{}", resp.text());

    let resp = h.request_with("GET", "/files/pkg?zip=1", b"", &auth);
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.header("content-type"), Some("application/zip"));
    let disposition = resp.header("content-disposition").unwrap_or_default();
    assert!(
        contains(disposition, "attachment") && contains(disposition, "pkg.zip"),
        "{}",
        disposition
    );
    let declared: usize = resp
        .header("content-length")
        .expect("length")
        .parse()
        .expect("numeric length");
    assert_eq!(resp.body.len(), declared, "body must match Content-Length");
    assert_eq!(&resp.body[..4], b"PK\x03\x04", "must start with a local header");
    assert_eq!(
        &resp.body[resp.body.len() - 22..resp.body.len() - 18],
        b"PK\x05\x06",
        "must end with the central directory record"
    );
    let raw = String::from_utf8_lossy(&resp.body).into_owned();
    for name in ["pkg/", "pkg/a.txt", "pkg/sub/", "pkg/sub/b.bin"] {
        assert!(raw.contains(name), "archive should contain {}", name);
    }
}

// A `Range` header on a generated archive is not supported, so the response
// must ignore it instead of advertising byte ranges it cannot honour.
#[test]
fn folder_download_ignores_range_requests() {
    let h = Harness::start();
    let cookie = h.login();
    let auth = [("Cookie", cookie.as_str())];
    let resp = h.request_with(
        "PUT",
        "/_api/upload?path=/&name=dir/f.txt&offset=0&total=2",
        b"hi",
        &auth,
    );
    assert_eq!(resp.status, 200, "{}", resp.text());

    let resp = h.request_with(
        "GET",
        "/files/dir?zip=1",
        b"",
        &[("Cookie", cookie.as_str()), ("Range", "bytes=0-3")],
    );
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.header("accept-ranges"), None);
    assert_eq!(&resp.body[..4], b"PK\x03\x04");
}

#[test]
fn method_and_route_guards() {
    let h = Harness::start();
    assert_eq!(h.request("DELETE", "/", b"").status, 405);
    assert_eq!(h.request("GET", "/_api/nope", b"").status, 404);
    let resp = h.request("OPTIONS", "/", b"");
    assert_eq!(resp.status, 204);
    assert_eq!(h.request("GET", "/files/missing.txt", b"").status, 404);
}

/// Reads exactly one response off a live socket, honouring `Content-Length`.
fn read_one(stream: &mut TcpStream) -> HttpResponse {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(split) = find(&buf, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..split]).to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if buf.len() >= split + 4 + len {
                return HttpResponse::parse(&buf);
            }
        }
        let n = stream.read(&mut chunk).expect("read response");
        assert!(n > 0, "connection closed before a full response arrived");
        buf.extend_from_slice(&chunk[..n]);
    }
}

#[test]
fn keep_alive_reuses_the_connection() {
    let h = Harness::start();
    let mut stream = TcpStream::connect(("127.0.0.1", h.port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();

    stream
        .write_all(b"GET /files/hello.txt HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .unwrap();
    let first = read_one(&mut stream);
    assert_eq!(first.status, 200);
    assert_eq!(first.text(), "hello world");
    assert_eq!(first.header("connection"), Some("keep-alive"));
    assert!(first.header("keep-alive").is_some());

    stream
        .write_all(b"GET /assets/app.js HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .unwrap();
    let second = read_one(&mut stream);
    assert_eq!(second.status, 200);
    assert_eq!(second.header("connection"), Some("keep-alive"));

    // `Connection: close` must end the loop, so the next read sees EOF.
    stream
        .write_all(b"GET /files/hello.txt HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .unwrap();
    let third = read_one(&mut stream);
    assert_eq!(third.status, 200);
    assert_eq!(third.header("connection"), Some("close"));
    let mut rest = Vec::new();
    assert_eq!(stream.read_to_end(&mut rest).unwrap(), 0);
    assert!(rest.is_empty());

    // HTTP/1.0 without an explicit token closes after one response.
    let mut old = TcpStream::connect(("127.0.0.1", h.port)).expect("connect");
    old.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    old.write_all(b"GET /files/hello.txt HTTP/1.0\r\n\r\n").unwrap();
    let resp = read_one(&mut old);
    assert_eq!(resp.status, 200);
    assert_eq!(resp.header("connection"), Some("close"));
}
