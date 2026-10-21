//! fern - a small HTTP file server.

// Release builds are windowless: a tray app should not drag a console along.
// Debug builds keep the console so `cargo run` prints where you expect it.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod api;
mod assets;
mod auth;
mod cli;
mod config;
mod fsapi;
mod http;
mod inflate;
mod json;
mod log;
mod mime;
mod platform;
mod range;
mod server;
mod trash;
mod util;
mod zip;

use config::Config;
use std::sync::Arc;

const VERSION: &str = concat!("fern ", env!("CARGO_PKG_VERSION"));

fn main() {
    platform::attach_console();
    let opts = cli::parse(std::env::args().skip(1));

    if opts.help {
        print!("{}", cli::usage());
        return;
    }
    if opts.version {
        println!("{}", VERSION);
        return;
    }
    if !opts.errors.is_empty() {
        for e in &opts.errors {
            platform::report_fatal(e);
        }
        eprintln!("try `fern --help`");
        std::process::exit(2);
    }

    let cfg = match Config::load(&opts) {
        Ok(c) => c,
        Err(e) => {
            platform::report_fatal(&e);
            std::process::exit(2);
        }
    };

    match run(cfg) {
        Ok(()) => {}
        Err(e) => {
            platform::report_fatal(&e);
            std::process::exit(1);
        }
    }
}

fn run(cfg: Config) -> Result<(), String> {
    log::init(&cfg);

    if let Some(action) = cfg.action {
        return platform::run_action(action, &cfg);
    }

    let cfg = Arc::new(cfg);
    if !cfg.root.is_dir() {
        log::warn(&format!(
            "root directory does not exist yet: {}",
            cfg.root.display()
        ));
    }

    if let Err(e) = platform::prepare(&cfg) {
        log::warn(&e);
    }

    let listeners = server::bind_all(&cfg).map_err(|e| format!("cannot listen: {}", e))?;
    load_mime_overrides(&cfg);
    let mut urls = Vec::new();
    for l in &listeners {
        match l.local_addr() {
            Ok(addr) => {
                let text = format_addr(addr);
                log::info(&format!("listening on http://{}", text));
                urls.push(text);
            }
            Err(_) => {}
        }
    }
    log::info(&format!("root: {}", cfg.root.display()));
    log::info(&format!(
        "guest permissions: {}",
        config::perms_to_letters(cfg.guest)
    ));
    for user in &cfg.users {
        log::info(&format!(
            "user {} permissions: {}",
            user.name,
            config::perms_to_letters(user.perms)
        ));
    }

    if cfg.trash_enabled && cfg.root.is_dir() {
        let removed = trash::cleanup(&cfg.root, &cfg.trash_policy());
        if removed > 0 {
            log::info(&format!("trash: removed {} expired entries", removed));
        }
    }

    let srv = Arc::new(server::Server::new(cfg.clone(), listeners));
    platform::after_start(&cfg, &urls, &srv);
    srv.run().map_err(|e| format!("server error: {}", e))
}

fn load_mime_overrides(cfg: &Config) {
    if cfg.mime_files.is_empty() {
        return;
    }
    let paths: Vec<&std::path::Path> = cfg.mime_files.iter().map(|p| p.as_path()).collect();
    mime::load_overrides(&paths);
    log::info(&format!("mime overrides: {}", cfg.mime_files.len()));
}

fn format_addr(addr: std::net::SocketAddr) -> String {
    let ip = addr.ip();
    let host = match ip {
        std::net::IpAddr::V4(v4) => {
            if v4.is_unspecified() {
                "127.0.0.1".to_string()
            } else {
                v4.to_string()
            }
        }
        std::net::IpAddr::V6(v6) => {
            if v6.is_unspecified() {
                "[::1]".to_string()
            } else {
                format!("[{}]", v6)
            }
        }
    };
    format!("{}:{}", host, addr.port())
}
