//! Users, permission checks and in-memory sessions.

use crate::config::{Config, User};
use crate::util::{base36, now_secs};
use std::collections::HashMap;
use std::sync::Mutex;

pub const SESSION_COOKIE: &str = "u";
pub const SESSION_TTL: u64 = 30 * 24 * 3600;
const LOGIN_WINDOW: u64 = 300;
const LOGIN_MAX_FAILURES: u32 = 10;

#[derive(Debug, Clone)]
pub struct Identity {
    pub name: String,
    pub perms: u32,
    pub logged_in: bool,
    pub token: Option<String>,
}

impl Identity {
    pub fn guest(perms: u32) -> Self {
        Identity {
            name: String::new(),
            perms,
            logged_in: false,
            token: None,
        }
    }

    pub fn can(&self, bit: u32) -> bool {
        self.perms & bit != 0
    }

}

#[derive(Debug, Clone)]
struct Entry {
    user: String,
    perms: u32,
    expires: u64,
}

pub struct Sessions {
    inner: Mutex<HashMap<String, Entry>>,
    failures: Mutex<HashMap<String, (u64, u32)>>,
}

impl Default for Sessions {
    fn default() -> Self {
        Self::new()
    }
}

impl Sessions {
    pub fn new() -> Self {
        Sessions {
            inner: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
        }
    }

    pub fn create(&self, user: &str, perms: u32) -> String {
        let token = random_token();
        if let Ok(mut map) = self.inner.lock() {
            map.retain(|_, e| e.expires > now_secs());
            map.insert(
                token.clone(),
                Entry {
                    user: user.to_string(),
                    perms,
                    expires: now_secs() + SESSION_TTL,
                },
            );
        }
        token
    }

    pub fn lookup(&self, token: &str) -> Option<(String, u32)> {
        let map = self.inner.lock().ok()?;
        let entry = map.get(token)?;
        if entry.expires <= now_secs() {
            return None;
        }
        Some((entry.user.clone(), entry.perms))
    }

    pub fn remove(&self, token: &str) {
        if let Ok(mut map) = self.inner.lock() {
            map.remove(token);
        }
    }

    /// Returns false when the caller has failed too often recently.
    pub fn allow_attempt(&self, key: &str) -> bool {
        let mut map = match self.failures.lock() {
            Ok(m) => m,
            Err(_) => return true,
        };
        let now = now_secs();
        map.retain(|_, (start, _)| now.saturating_sub(*start) < LOGIN_WINDOW * 4);
        match map.get(key) {
            Some((start, count)) => {
                if now.saturating_sub(*start) > LOGIN_WINDOW {
                    map.insert(key.to_string(), (now, 1));
                    true
                } else {
                    *count < LOGIN_MAX_FAILURES
                }
            }
            None => {
                map.insert(key.to_string(), (now, 1));
                true
            }
        }
    }

    pub fn note_failure(&self, key: &str) {
        if let Ok(mut map) = self.failures.lock() {
            let now = now_secs();
            let entry = map.entry(key.to_string()).or_insert((now, 0));
            entry.1 = entry.1.saturating_add(1);
        }
    }

    pub fn clear_failures(&self, key: &str) {
        if let Ok(mut map) = self.failures.lock() {
            map.remove(key);
        }
    }
}

pub fn identify(cfg: &Config, sessions: &Sessions, token: Option<String>) -> Identity {
    if let Some(t) = token.as_deref() {
        if let Some((user, perms)) = sessions.lookup(t) {
            return Identity {
                name: user,
                perms,
                logged_in: true,
                token: token.clone(),
            };
        }
    }
    Identity::guest(cfg.guest)
}

/// Constant time comparison, avoids leaking password length and prefix.
pub fn secret_eq(a: &str, b: &str) -> bool {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let mut diff = (ab.len() ^ bb.len()) as u8;
    let max = ab.len().max(bb.len());
    for i in 0..max {
        let x = *ab.get(i).unwrap_or(&0);
        let y = *bb.get(i).unwrap_or(&0);
        diff |= x ^ y;
    }
    diff == 0
}

pub fn authenticate<'a>(cfg: &'a Config, user: &str, pass: &str) -> Option<&'a User> {
    cfg.user(user)
        .filter(|u| secret_eq(&u.pass, pass))
        .filter(|u| u.perms != 0)
}

pub fn random_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::with_capacity(32);
    for round in 0..4u64 {
        let state = RandomState::new();
        let mut h = state.build_hasher();
        h.write_u64(now_secs());
        h.write_u64(round);
        h.write_u32(std::process::id());
        let v = h.finish();
        out.push_str(&base36(v));
        out.push('.');
    }
    out.pop();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_expire_only_for_unknown() {
        let s = Sessions::new();
        let t = s.create("alice", 7);
        assert_eq!(s.lookup(&t).unwrap().0, "alice");
        s.remove(&t);
        assert!(s.lookup(&t).is_none());
    }

    #[test]
    fn secret_compare() {
        assert!(secret_eq("abc", "abc"));
        assert!(!secret_eq("abc", "abd"));
        assert!(!secret_eq("abc", "abcd"));
    }

    #[test]
    fn tokens_are_unique() {
        let a = random_token();
        let b = random_token();
        assert_ne!(a, b);
        assert!(!a.is_empty());
    }
}
