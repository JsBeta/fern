//! Fallback integration for platforms without a dedicated module.

use super::{check_only, default_url, open_browser, unsupported};
use crate::cli::Action;
use crate::config::Config;
use crate::server::Server;
use std::sync::Arc;

pub fn prepare(cfg: &Config) -> Result<(), String> {
    if !cfg.root.exists() {
        std::fs::create_dir_all(&cfg.root)
            .map_err(|e| format!("cannot create root {}: {}", cfg.root.display(), e))?;
    }
    Ok(())
}

pub fn after_start(cfg: &Config, urls: &[String], _server: &Arc<Server>) {
    if !cfg.open_browser {
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
