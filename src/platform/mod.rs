//! Platform glue: service/firewall management, daemon mode, browser launch.

use crate::cli::Action;
use crate::config::Config;
use crate::server::Server;
use std::sync::Arc;

#[cfg(windows)]
mod console_win;
#[cfg(windows)]
mod win;
#[cfg(windows)]
mod tray_win;
#[cfg(windows)]
pub use win::{after_start, prepare, run_action};

#[cfg(windows)]
pub use console_win::{attach as attach_console, report as report_fatal};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{after_start, prepare, run_action};

#[cfg(not(any(windows, unix)))]
mod other;
#[cfg(not(any(windows, unix)))]
pub use other::{after_start, prepare, run_action};

/// No-op: only the Windows build is windowless.
#[cfg(not(windows))]
pub fn attach_console() {}

/// Writes a fatal startup error to stderr.
#[cfg(not(windows))]
pub fn report_fatal(message: &str) {
    eprintln!("fern: {}", message);
}

/// Builds a command for a helper process. A windowless build owns no console,
/// so children have to be told not to open one for themselves.
pub fn command(program: &str) -> std::process::Command {
    #[allow(unused_mut)]
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(console_win::CREATE_NO_WINDOW);
    }
    cmd
}

#[allow(dead_code)]
pub fn open_browser(url: &str) {
    #[cfg(windows)]
    {
        let _ = command("cmd").args(["/c", "start", "", url]).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = command("open").arg(url).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = command("xdg-open").arg(url).spawn();
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = url;
    }
}

#[allow(dead_code)]
pub fn executable_path() -> String {
    std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "fern".to_string())
}

#[allow(dead_code)]
pub fn unsupported(action: Action) -> Result<(), String> {
    Err(format!(
        "{} is not supported on this platform",
        action_name(action)
    ))
}

#[allow(dead_code)]
pub fn action_name(action: Action) -> &'static str {
    match action {
        Action::InstallService => "--install-service",
        Action::UninstallService => "--uninstall-service",
        Action::AddFirewall => "--add-firewall",
        Action::RemoveFirewall => "--remove-firewall",
        Action::Check => "--check",
    }
}

#[allow(dead_code)]
pub fn check_only(cfg: &Config) -> Result<(), String> {
    println!("configuration is valid");
    if let Some(p) = &cfg.config_path {
        println!("  config file : {}", p.display());
    }
    println!("  root        : {}", cfg.root.display());
    println!("  listen      : {}", cfg.listen.join(", "));
    println!(
        "  guest perms : {}",
        crate::config::perms_to_letters(cfg.guest)
    );
    for u in &cfg.users {
        println!(
            "  user        : {} ({})",
            u.name,
            crate::config::perms_to_letters(u.perms)
        );
    }
    println!("  max conns   : {}", cfg.max_conns);
    println!("  title       : {}", cfg.title);
    if !cfg.notice.is_empty() {
        println!("  notice      : {}", cfg.notice);
    }
    Ok(())
}

#[allow(dead_code)]
pub fn default_url(urls: &[String]) -> Option<String> {
    urls.first().map(|u| format!("http://{}/", u))
}

#[allow(dead_code)]
pub fn keep_alive(server: &Arc<Server>) -> Arc<Server> {
    server.clone()
}
