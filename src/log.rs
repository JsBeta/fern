//! Console, daily file and syslog logging.

use crate::config::Config;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

struct Sink {
    file: Option<(PathBuf, File)>,
    log_dir: Option<PathBuf>,
    console: bool,
    syslog: bool,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

pub fn init(cfg: &Config) {
    let console = !cfg.hide && cfg.log_dir.is_none();
    let sink = Sink {
        file: None,
        log_dir: cfg.log_dir.clone(),
        console,
        syslog: cfg.syslog,
    };
    let _ = SINK.set(Mutex::new(sink));
}

pub fn info(msg: &str) {
    emit("INFO", msg);
}

pub fn error(msg: &str) {
    emit("ERROR", msg);
}

pub fn warn(msg: &str) {
    emit("WARN", msg);
}

fn emit(level: &str, msg: &str) {
    let text = format!("[{}] {} {}\n", level, crate::util::http_date(crate::util::now_secs()), msg);
    let sink = match SINK.get() {
        Some(s) => s,
        None => {
            eprint!("{}", text);
            return;
        }
    };
    let mut guard = match sink.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if guard.console {
        eprint!("{}", text);
    }
    if guard.syslog {
        platform_syslog(&text);
    }
    if let Some(dir) = guard.log_dir.clone() {
        let name = log_file_name();
        let stale = match &guard.file {
            Some((path, _)) => path.file_name().and_then(|s| s.to_str()) != Some(name.as_str()),
            None => true,
        };
        if stale {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                if guard.console {
                    eprintln!("[ERROR] cannot create log dir {}: {}", dir.display(), e);
                }
            } else {
                let path = dir.join(&name);
                match OpenOptions::new().create(true).append(true).open(&path) {
                    Ok(f) => guard.file = Some((path, f)),
                    Err(e) => {
                        if guard.console {
                            eprintln!("[ERROR] cannot open {}: {}", dir.display(), e);
                        }
                    }
                }
            }
        }
        if let Some((_, f)) = guard.file.as_mut() {
            let _ = f.write_all(text.as_bytes());
            let _ = f.flush();
        }
    }
}

fn log_file_name() -> String {
    let now = crate::util::now_secs() as i64;
    let days = now.div_euclid(86_400);
    let (y, m, d) = civil(days);
    format!("fern-{:04}-{:02}-{:02}.log", y, m, d)
}

fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

#[cfg(unix)]
fn platform_syslog(text: &str) {
    use std::ffi::CString;
    use std::os::raw::c_char;
    extern "C" {
        fn openlog(ident: *const c_char, option: i32, facility: i32);
        fn syslog(priority: i32, format: *const c_char, ...) -> ();
    }
    let msg = text.trim_end();
    let Ok(c) = CString::new(msg) else { return };
    let Ok(fmt) = CString::new("%s") else { return };
    unsafe {
        openlog(std::ptr::null(), 0, 0);
        syslog(6, fmt.as_ptr(), c.as_ptr());
    }
}

#[cfg(not(unix))]
fn platform_syslog(_text: &str) {}
