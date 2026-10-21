//! Command line parsing. Flags follow the original `webd` binary; extra
//! long options are additive and never clash with the short ones.

use crate::config::{parse_perms, User};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    InstallService,
    UninstallService,
    AddFirewall,
    RemoveFirewall,
    Check,
}

#[derive(Debug, Default)]
pub struct Options {
    pub config: Option<PathBuf>,
    pub root: Option<String>,
    pub listen: Vec<String>,
    pub guest: Option<u32>,
    pub users: Vec<User>,
    pub hide: bool,
    pub open_browser: bool,
    pub help: bool,
    pub version: bool,
    pub action: Option<Action>,
    pub log_dir: Option<PathBuf>,
    pub max_conns: Option<usize>,
    pub errors: Vec<String>,
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Options {
    let mut o = Options {
        open_browser: true,
        ..Default::default()
    };
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-c" => match it.next() {
                Some(v) => o.config = Some(PathBuf::from(v)),
                None => o.errors.push("-c requires a file path".into()),
            },
            "-w" => match it.next() {
                Some(v) => o.root = Some(v),
                None => o.errors.push("-w requires a directory".into()),
            },
            "-l" => match it.next() {
                Some(v) => o.listen.push(v),
                None => o.errors.push("-l requires an address".into()),
            },
            "-g" => match it.next().as_deref().and_then(parse_perms) {
                Some(p) => o.guest = Some(p),
                None => o.errors.push("-g requires permission letters (RLUMSTD) or 0".into()),
            },
            "-u" => match it.next() {
                Some(v) => match parse_user_spec(&v) {
                    Some(u) => o.users.push(u),
                    None => o
                        .errors
                        .push("-u expects user:password:permissions".into()),
                },
                None => o.errors.push("-u expects user:password:permissions".into()),
            },
            "-h" => o.hide = true,
            "-B" => o.open_browser = false,
            "--max-conns" => match it.next().as_deref().and_then(|v| v.parse().ok()) {
                Some(n) => o.max_conns = Some(n),
                None => o.errors.push("--max-conns requires a number".into()),
            },
            "--log-dir" => match it.next() {
                Some(v) => o.log_dir = Some(PathBuf::from(v)),
                None => o.errors.push("--log-dir requires a directory".into()),
            },
            "--install-service" => o.action = Some(Action::InstallService),
            "--uninstall-service" => o.action = Some(Action::UninstallService),
            "--add-firewall" => o.action = Some(Action::AddFirewall),
            "--remove-firewall" => o.action = Some(Action::RemoveFirewall),
            "--check" => o.action = Some(Action::Check),
            "--help" | "-?" => o.help = true,
            "--version" | "-v" => o.version = true,
            // A bare argument is taken as the configuration file, so that
            // `fern --check ./fern.conf` works like `fern -c ./fern.conf --check`.
            other if !other.starts_with('-') => {
                if o.config.is_some() {
                    o.errors.push(format!("unexpected argument: {}", other));
                } else {
                    o.config = Some(PathBuf::from(other));
                }
            }
            other => o.errors.push(format!("unknown option: {}", other)),
        }
    }
    o
}

fn parse_user_spec(spec: &str) -> Option<User> {
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let (perms, user, pass) = if parse_perms(parts[0]).is_some() {
        (parse_perms(parts[0])?, parts[1], parts[2])
    } else if parse_perms(parts[2]).is_some() {
        (parse_perms(parts[2])?, parts[0], parts[1])
    } else {
        return None;
    };
    if user.is_empty() || pass.is_empty() {
        return None;
    }
    Some(User {
        name: user.to_string(),
        pass: pass.to_string(),
        perms,
    })
}

pub fn usage() -> &'static str {
    "fern - tiny HTTP file server

Usage: fern [options]

  -c <file>        configuration file (default: ./fern.conf or /etc/fern.conf)
                   a bare argument (e.g. `fern --check ./fern.conf`) also works
  -w <dir>         shared root directory (default: the folder holding fern.exe
                   when no configuration file exists, otherwise ./web)
  -l <addr>        listen address, repeatable (default: 4545)
                   forms: 4545 | 127.0.0.1:4545 | [::]:4545
  -g <perms>       guest permissions using letters RLUMSTD, or 0 to disable
  -u <spec>        add a user: user:password:permissions
  -h               hide the tray icon (this is not \"help\")
  -B               do not open the browser at startup
  --max-conns <n>  maximum simultaneous connections (default 128)
  --log-dir <dir>  write daily log files into <dir>
  --install-service / --uninstall-service      Windows service management
  --add-firewall / --remove-firewall           Windows firewall rules
  --check          validate the configuration and exit
  --help           show this text
  --version        show the version

Permission letters (uppercase; lowercase is accepted too):
  R read files     L list directories   U upload
  M modify/rename/delete               S show hidden entries
  T web playback   D download hint

Environment (Unix and Windows where applicable):
  _LOG_DIR  _syslog  _FD_LIMIT  _RUNAS  _CHROOT_PATH"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn bare_argument_is_the_config_file() {
        let o = parse(args(&["--check", "./fern.conf"]));
        assert!(o.errors.is_empty());
        assert_eq!(o.action, Some(Action::Check));
        assert_eq!(o.config, Some(PathBuf::from("./fern.conf")));
    }

    #[test]
    fn bare_argument_and_dash_c_conflict() {
        let o = parse(args(&["-c", "a.conf", "b.conf"]));
        assert_eq!(o.config, Some(PathBuf::from("a.conf")));
        assert_eq!(o.errors.len(), 1);
    }

    #[test]
    fn unknown_option_still_rejected() {
        let o = parse(args(&["--nope"]));
        assert_eq!(o.errors, vec!["unknown option: --nope".to_string()]);
    }
}
