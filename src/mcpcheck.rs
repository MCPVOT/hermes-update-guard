//! MCP server preflight: is every ENABLED MCP server actually usable, and will it
//! work in a headless (gateway/cron) context?
//!
//! WHY
//!   On 2026-10-21 two OAuth MCP servers (webflow on default, onlyfans on ofcrm)
//!   logged ~1,200 failures in 16 hours: the gateway and cron run non-interactively,
//!   so an expired or never-registered OAuth token can never complete an
//!   authorisation flow and every call to that server fails. It also blew the
//!   mcpvot runtime readiness budget, which reads as a slow profile rather than a
//!   broken credential.
//!
//! WHAT IT CHECKS (per profile, read-only, no network)
//!   - which servers are enabled
//!   - whether an OAuth token file exists, and whether it is EXPIRED
//!   - whether the OAuth client was ever registered (no client.json)
//!   - a stale refresh lock left behind by a dead process
//!
//! A server that needs interactive re-auth is reported as `needs-interactive-auth`:
//! the fix is one interactive `hermes mcp login/reauth`, or disabling the server
//! for background use.

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::env;

/// One MCP server's readiness.
#[derive(Debug, Clone)]
pub struct Server {
    pub profile: String,
    pub name: String,
    pub auth: String,
    pub enabled: bool,
    /// none | ok | expired | missing | unknown
    pub token_state: String,
    pub expires_at: String,
    pub note: String,
}

impl Server {
    /// A server that will fail in a headless context.
    ///
    /// Only OAuth servers can be "broken" this way. An `auth: none` / stdio server
    /// never has a token file, so judging it by token state reported every such
    /// server as broken — the check cried wolf on a healthy config.
    pub fn is_broken(&self) -> bool {
        self.enabled
            && self.auth.eq_ignore_ascii_case("oauth")
            && matches!(self.token_state.as_str(), "expired" | "missing" | "unknown")
    }
}

/// The root token store.
fn token_dir(home: &Path) -> PathBuf {
    home.join("mcp-tokens")
}

/// Stores to search for a profile's tokens, most specific first.
///
/// A profile keeps its own tokens under `profiles/<p>/mcp-tokens` and only falls
/// back to the root store. Reading the root only reported every per-profile OAuth
/// server as "no client registered" — a loud false alarm, which is how a real one
/// gets missed.
fn token_stores(home: &Path, profile: &str) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if profile != "default" {
        v.push(home.join("profiles").join(profile).join("mcp-tokens"));
    }
    v.push(token_dir(home));
    v
}

/// A refresh lock older than this is treated as stale (a live refresh is seconds).
const LOCK_STALE_SECS: u64 = 600;

fn lock_is_stale(lock: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(lock) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    match modified.elapsed() {
        Ok(age) => age.as_secs() > LOCK_STALE_SECS,
        Err(_) => false, // clock skew: do not claim stale without evidence
    }
}

/// Read the token state for a server in one profile.
///
/// Returns `(token_state, expires_at, client_registered, stale_lock, lock_present)`.
/// An `expires_at` that exists but cannot be parsed is `unknown` (fail closed):
/// reporting `ok` for a value we could not read is how an expired token passes.
fn token_state(home: &Path, profile: &str, name: &str) -> (String, String, bool, bool, bool) {
    let mut found: Option<(PathBuf, PathBuf, PathBuf)> = None;
    for dir in token_stores(home, profile) {
        let tok = dir.join(format!("{}.json", name));
        let client = dir.join(format!("{}.client.json", name));
        if tok.exists() || client.exists() {
            found = Some((tok, client, dir));
            break;
        }
    }
    let Some((tok, client, dir)) = found else {
        return ("missing".into(), String::new(), false, false, false);
    };

    let lock = dir.join(format!("{}.json.refresh.lock", name));
    let lock_present = lock.exists();
    let stale_lock = lock_present && lock_is_stale(&lock);
    let client_registered = client.exists();

    if !tok.exists() {
        return (
            "missing".into(),
            String::new(),
            client_registered,
            stale_lock,
            lock_present,
        );
    }
    let Ok(text) = std::fs::read_to_string(&tok) else {
        return (
            "unknown".into(),
            String::new(),
            client_registered,
            stale_lock,
            lock_present,
        );
    };

    // Parse the token file as JSON. A line-scanner only saw pretty-printed files
    // and silently reported `ok` for a single-line or malformed one — the exact
    // fail-open direction that lets an expired token pass.
    let mut expires = String::new();
    let mut state = "ok".to_string();
    match serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}')) {
        Ok(v) => {
            if let Some(e) = v.get("expires_at") {
                let raw = match e {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    other => other.to_string(),
                };
                match raw.parse::<f64>() {
                    Ok(secs) => {
                        if secs <= now_secs() {
                            state = "expired".into();
                        }
                    }
                    Err(_) => state = "unknown".into(),
                }
                expires = raw;
            }
        }
        Err(_) => state = "unknown".into(),
    }
    (state, expires, client_registered, stale_lock, lock_present)
}

/// Parse a Hermes config.yaml well enough to find `mcp_servers`.
fn mcp_servers(home: &Path, profile: &str) -> Vec<(String, String, bool, String)> {
    let cfg = env::config_yaml(home, profile);
    let Ok(text) = std::fs::read_to_string(&cfg) else {
        return Vec::new();
    };
    // A tiny, dependency-free reader for the shape we need:
    //   mcp_servers:
    //     <name>:
    //       url: ...
    //       auth: oauth
    //       enabled: false
    let mut out: Vec<(String, String, bool, String)> = Vec::new();
    let mut in_block = false;
    let mut cur: Option<(String, String, bool, String)> = None;
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let body = line.trim();
        if indent == 0 {
            if in_block {
                if let Some(c) = cur.take() {
                    out.push(c);
                }
                in_block = false;
            }
            if body.starts_with("mcp_servers:") {
                in_block = true;
            }
            continue;
        }
        if !in_block {
            continue;
        }
        if indent == 2 && body.ends_with(':') {
            if let Some(c) = cur.take() {
                out.push(c);
            }
            let name = body
                .trim_end_matches(':')
                .trim()
                .trim_matches('"')
                .to_string();
            cur = Some((name, "none".into(), true, String::new()));
            continue;
        }
        if let Some(c) = cur.as_mut() {
            if let Some(rest) = body.strip_prefix("auth:") {
                c.1 = rest.trim().trim_matches('"').to_string();
            } else if let Some(rest) = body.strip_prefix("enabled:") {
                let v = rest.trim().trim_matches('"').to_ascii_lowercase();
                c.2 = v != "false" && v != "no";
            } else if let Some(rest) = body.strip_prefix("url:") {
                c.3 = rest.trim().to_string();
            }
        }
    }
    if let Some(c) = cur.take() {
        out.push(c);
    }
    out
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Audit one profile's MCP servers.
pub fn audit_profile(home: &Path, profile: &str) -> Vec<Server> {
    let mut out = Vec::new();
    for (name, auth, enabled, _url) in mcp_servers(home, profile) {
        let (state, expires, client, stale_lock, lock_present) = token_state(home, profile, &name);
        let mut note = String::new();
        if enabled && auth.eq_ignore_ascii_case("oauth") {
            match state.as_str() {
                "expired" => {
                    note = "token expired - needs an interactive `hermes mcp reauth`".into()
                }
                "missing" => {
                    if client {
                        note = "client registered but no token - needs an interactive `hermes mcp login`"
                            .into()
                    } else {
                        note =
                            "no OAuth client registered - needs an interactive `hermes mcp login`"
                                .into()
                    }
                }
                "unknown" => {
                    note = "token file unreadable or expiry unparseable - re-auth to be safe".into()
                }
                _ => {}
            }
            if stale_lock {
                note.push_str("; stale refresh lock present");
            } else if lock_present {
                note.push_str("; refresh lock present (fresh)");
            }
        }
        out.push(Server {
            profile: profile.to_string(),
            name,
            auth,
            enabled,
            token_state: state,
            expires_at: expires,
            note,
        });
    }
    out
}

/// Audit the fleet.
pub fn audit(home: &Path) -> Vec<Server> {
    let mut out = Vec::new();
    for p in env::profiles(home) {
        out.extend(audit_profile(home, &p));
    }
    out
}

/// `exp=<date>` for a parseable Unix expiry, `exp=?` when it could not be parsed,
/// nothing when the token carries no expiry at all.
fn expiry_hint(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    match raw.parse::<f64>() {
        Ok(secs) => match chrono::DateTime::from_timestamp(secs as i64, 0) {
            Some(d) => format!(" exp={}", d.format("%Y-%m-%d")),
            None => " exp=?".into(),
        },
        Err(_) => " exp=?".into(),
    }
}

/// Print the fleet MCP readiness. Returns true when something is broken.
pub fn report(home: &Path) -> Result<bool> {
    let servers = audit(home);
    println!("== MCP READINESS ==");
    if servers.is_empty() {
        println!("  no MCP servers configured");
        return Ok(false);
    }
    let mut broken = 0;
    for s in &servers {
        let state = if !s.enabled {
            "disabled".to_string()
        } else {
            s.token_state.clone()
        };
        let flag = if s.is_broken() { "!" } else { " " };
        if s.is_broken() {
            broken += 1;
        }
        println!(
            "  {} {:<10} {:<18} auth={:<6} token={:<8}{}{}",
            flag,
            s.profile,
            s.name,
            s.auth,
            state,
            expiry_hint(&s.expires_at),
            if s.note.is_empty() {
                String::new()
            } else {
                format!("  <- {}", s.note)
            }
        );
    }
    if broken > 0 {
        println!(
            "  [FAIL] {} enabled MCP server(s) cannot authenticate headlessly - they will fail every call",
            broken
        );
    } else {
        println!("  [ok]   every enabled MCP server has usable credentials");
    }
    Ok(broken > 0)
}

/// Exit-code entry point.
pub fn run(home: &Path) -> Result<()> {
    if report(home)? {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srv(auth: &str, state: &str, enabled: bool) -> Server {
        Server {
            profile: "p".into(),
            name: "n".into(),
            auth: auth.into(),
            enabled,
            token_state: state.into(),
            expires_at: String::new(),
            note: String::new(),
        }
    }

    /// The bug this replaced: an enabled `auth: none` server has no token file, so
    /// it was reported broken on a perfectly healthy config.
    #[test]
    fn non_oauth_servers_are_never_broken_for_missing_tokens() {
        assert!(!srv("none", "missing", true).is_broken());
        assert!(!srv("stdio", "missing", true).is_broken());
        assert!(
            !srv("oauth", "missing", false).is_broken(),
            "disabled is fine"
        );
        assert!(srv("oauth", "missing", true).is_broken());
        assert!(srv("OAuth", "expired", true).is_broken());
        assert!(!srv("oauth", "ok", true).is_broken());
    }

    fn home(tag: &str) -> std::path::PathBuf {
        let h = std::env::temp_dir().join(format!("hug-mcp-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(&h).unwrap();
        h
    }

    /// A profile's own token store must be searched before the root store;
    /// reading the root only reported real per-profile OAuth servers as missing.
    #[test]
    fn per_profile_token_store_is_found() {
        let h = home("perprofile");
        let d = h.join("profiles/ofcrm/mcp-tokens");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("onlyfans.client.json"), "{}").unwrap();
        std::fs::write(
            d.join("onlyfans.json"),
            "{\"expires_at\": \"9999999999.0\"}\n",
        )
        .unwrap();
        let (state, exp, client, _stale, _present) = token_state(&h, "ofcrm", "onlyfans");
        assert_eq!(state, "ok");
        assert!(client, "client must be seen in the profile store");
        assert_eq!(exp, "9999999999.0");
        let _ = std::fs::remove_dir_all(&h);
    }

    /// An expiry we cannot parse must not read as valid.
    #[test]
    fn unparseable_expiry_is_unknown_not_ok() {
        let h = home("bad-exp");
        let d = h.join("mcp-tokens");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("x.json"), "{\"expires_at\": \"next-tuesday\"}\n").unwrap();
        let (state, _, _, _, _) = token_state(&h, "default", "x");
        assert_eq!(state, "unknown", "fail closed: cannot prove it is valid");
        let _ = std::fs::remove_dir_all(&h);
    }

    /// An expired token is expired.
    #[test]
    fn expired_token_is_detected() {
        let h = home("expired");
        let d = h.join("mcp-tokens");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("y.json"), "{\"expires_at\": \"1000000000.0\"}\n").unwrap();
        let (state, _, _, _, _) = token_state(&h, "default", "y");
        assert_eq!(state, "expired");
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn expiry_hint_formats_or_admits_it_cannot() {
        assert_eq!(expiry_hint(""), "");
        assert_eq!(expiry_hint("1700000000"), " exp=2023-11-14");
        assert_eq!(expiry_hint("nonsense"), " exp=?");
    }
}
