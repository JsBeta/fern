//! Unix integration: daemon helpers, chroot, resource limits, browser launch.

use super::{check_only, default_url, open_browser, unsupported};
use crate::cli::Action;
use crate::config::Config;
use crate::server::Server;
use std::os::raw::{c_char, c_int, c_uint, c_ulong};
use std::sync::Arc;

#[cfg(any(target_os = "linux", target_os = "android"))]
const RLIMIT_NOFILE: c_int = 7;
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
const RLIMIT_NOFILE: c_int = 8;
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
)))]
const RLIMIT_NOFILE: c_int = 7;

/// `struct rlimit` from glibc: `rlim_t` is `unsigned long`, which is 32-bit on
/// armv7 and 64-bit on x86_64, so the fields must not be hard-coded to `u64`.
#[repr(C)]
struct Rlimit {
    rlim_cur: c_ulong,
    rlim_max: c_ulong,
}

extern "C" {
    fn setrlimit(resource: c_int, rlim: *const Rlimit) -> c_int;
    fn chroot(path: *const c_char) -> c_int;
    fn setgid(gid: c_uint) -> c_int;
    fn setuid(uid: c_uint) -> c_int;
    fn getuid() -> c_uint;
}

pub fn prepare(cfg: &Config) -> Result<(), String> {
    if let Some(limit) = cfg.fd_limit {
        let value = c_ulong::try_from(limit).map_err(|_| {
            format!(
                "_FD_LIMIT {} exceeds the platform maximum of {}",
                limit,
                c_ulong::MAX
            )
        })?;
        let rl = Rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        let rc = unsafe { setrlimit(RLIMIT_NOFILE, &rl) };
        if rc != 0 {
            return Err(format!(
                "cannot raise the open file limit to {}: {}",
                limit,
                std::io::Error::last_os_error()
            ));
        }
    }

    if let Some(chroot_dir) = &cfg.chroot {
        let text = chroot_dir.display().to_string();
        let c_path = std::ffi::CString::new(text.clone())
            .map_err(|_| "chroot path contains a NUL byte".to_string())?;
        if unsafe { chroot(c_path.as_ptr()) } != 0 {
            return Err(format!(
                "cannot chroot to {}: {}",
                text,
                std::io::Error::last_os_error()
            ));
        }
        std::env::set_current_dir("/").map_err(|e| format!("chdir after chroot: {}", e))?;
    }

    if let Some(user) = &cfg.runas {
        let uid: c_uint = user
            .parse()
            .map_err(|_| format!("_RUNAS expects a numeric uid, got {:?}", user))?;
        if unsafe { getuid() } != 0 {
            return Err("_RUNAS requires root privileges".into());
        }
        if unsafe { setgid(uid) } != 0 || unsafe { setuid(uid) } != 0 {
            return Err(format!(
                "cannot switch to uid {}: {}",
                uid,
                std::io::Error::last_os_error()
            ));
        }
    }

    Ok(())
}

pub fn after_start(cfg: &Config, urls: &[String], _server: &Arc<Server>) {
    if !cfg.open_browser || std::env::var_os("DISPLAY").is_none() {
        return;
    }
    if let Some(url) = default_url(urls) {
        open_browser(&url);
    }
}

pub fn run_action(action: Action, cfg: &Config) -> Result<(), String> {
    match action {
        Action::Check => check_only(cfg),
        other => unsupported(other),
    }
}
