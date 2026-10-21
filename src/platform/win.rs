//! Windows specific integration: firewall rules, service management, browser launch.

use super::{action_name, check_only, default_url, executable_path, open_browser};
use crate::cli::Action;
use crate::config::Config;
use crate::server::Server;
use std::sync::Arc;

const SERVICE_NAME: &str = "fern";
const RULE_NAME: &str = "fern";

pub fn prepare(cfg: &Config) -> Result<(), String> {
    if !cfg.root.exists() {
        std::fs::create_dir_all(&cfg.root)
            .map_err(|e| format!("cannot create root {}: {}", cfg.root.display(), e))?;
    }
    Ok(())
}

pub fn after_start(cfg: &Config, urls: &[String], _server: &Arc<Server>) {
    let url = default_url(urls);
    if !cfg.hide {
        if let Some(url) = &url {
            let tip = format!("fern - {}", url);
            crate::platform::tray_win::spawn(url.clone(), tip);
        }
    }
    if !cfg.open_browser {
        return;
    }
    if let Some(url) = url {
        open_browser(&url);
    }
}

pub fn run_action(action: Action, cfg: &Config) -> Result<(), String> {
    match action {
        Action::Check => check_only(cfg),
        Action::AddFirewall => firewall(true),
        Action::RemoveFirewall => firewall(false),
        Action::InstallService => install_service(cfg),
        Action::UninstallService => uninstall_service(),
    }
}

fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let output = super::command(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run {}: {}", cmd, e))?;
    let mut text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !err.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&err);
    }
    if output.status.success() {
        Ok(text)
    } else {
        Err(format!("{} {} failed: {}", cmd, args.join(" "), text))
    }
}

fn firewall(add: bool) -> Result<(), String> {
    let exe = executable_path();
    if add {
        run(
            "netsh",
            &[
                "advfirewall",
                "firewall",
                "add",
                "rule",
                &format!("name={}", RULE_NAME),
                "dir=in",
                "action=allow",
                &format!("program={}", exe),
                "enable=yes",
                "profile=any",
            ],
        )?;
        println!("firewall rule `{}` added for {}", RULE_NAME, exe);
    } else {
        run(
            "netsh",
            &[
                "advfirewall",
                "firewall",
                "delete",
                "rule",
                &format!("name={}", RULE_NAME),
            ],
        )?;
        println!("firewall rule `{}` removed", RULE_NAME);
    }
    Ok(())
}

fn install_service(cfg: &Config) -> Result<(), String> {
    let exe = executable_path();
    let mut bin = format!("\"{}\"", exe);
    if let Some(p) = &cfg.config_path {
        bin.push_str(&format!(" -c \"{}\"", p.display()));
    }
    let _ = run("sc.exe", &["stop", SERVICE_NAME]);
    let _ = run("sc.exe", &["delete", SERVICE_NAME]);
    run(
        "sc.exe",
        &[
            "create",
            SERVICE_NAME,
            "DisplayName=",
            "fern file server",
            "start=",
            "auto",
            "binPath=",
            &bin,
        ],
    )?;
    println!("service `{}` installed", SERVICE_NAME);
    Ok(())
}

fn uninstall_service() -> Result<(), String> {
    let _ = run("sc.exe", &["stop", SERVICE_NAME]);
    run("sc.exe", &["delete", SERVICE_NAME])?;
    println!("service `{}` removed", SERVICE_NAME);
    Ok(())
}

#[allow(dead_code)]
pub fn describe(action: Action) -> &'static str {
    action_name(action)
}
