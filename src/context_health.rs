//! Context-health audit: the class of failure that makes sessions die with
//! "context length exceeded" long before anyone looks.
//!
//! WHY THIS EXISTS
//!   On 2026-10-19 we found a Hermes fleet where EVERY live message was stored
//!   TWICE in `messages` (2 active rows per `message_uid`, identical content,
//!   identical `display_order`). The transcript sent to the model was therefore
//!   ~2x reality, so sessions hit the model's context wall twice as early, and
//!   each compaction cycle re-archived the same rows (37 copies of one tool
//!   output in one session). This was invisible for five weeks because the
//!   tripwire only watched the *provider surface* - never the transcript.
//!
//! WHAT IT CHECKS (per profile, read-only)
//!   1. duplicate live rows: >1 active row for the same `message_uid`
//!   2. archive bloat: total rows vs distinct logical messages
//!   3. sessions with a recorded compression failure / cooldown
//!   4. DB file size (a DB over Hermes' own 1 GB snapshot limit is silently
//!      skipped by `hermes update`'s built-in snapshot)
//!
//! It is a DIAGNOSTIC, never a writer. Exit code 1 = problems found.
//!
//! The profile list is DISCOVERED (`env::profiles`), never hardcoded: this binary
//! ships publicly and must not carry one operator's profile names.

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::env;

/// A comparable snapshot of the fleet's context health.
///
/// Taken BEFORE an update and compared AFTER it, so a failure can be attributed
/// correctly: a transcript that was already broken (the 2026-10-19 vot session)
/// must not make the update look like it caused the damage.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Summary {
    pub dup_uids: i64,
    pub max_bloat: f64,
    pub stuck: i64,
    pub at_risk: i64,
}

impl Summary {
    /// True when `self` is materially worse than `before`.
    pub fn is_worse_than(&self, before: &Summary) -> bool {
        self.dup_uids > before.dup_uids
            || self.max_bloat > before.max_bloat + 0.5
            || self.stuck > before.stuck
            || self.at_risk > before.at_risk
    }

    pub fn same_as(&self, before: &Summary) -> bool {
        !self.is_worse_than(before) && !before.is_worse_than(self)
    }
}

/// Snapshot the fleet's context health for later comparison.
pub fn summarize(home: &Path) -> Summary {
    let mut s = Summary::default();
    for h in audit(home) {
        s.dup_uids += h.dup_uids;
        s.stuck += h.stuck_sessions.len() as i64;
        s.at_risk += h.at_risk.len() as i64;
        if h.bloat_ratio() > s.max_bloat {
            s.max_bloat = h.bloat_ratio();
        }
    }
    s
}

/// One profile's context-health findings.
#[derive(Debug, Default)]
pub struct Health {
    pub profile: String,
    pub db_bytes: u64,
    pub db_mb: f64,
    pub live_rows: i64,
    pub distinct_live: i64,
    pub total_rows: i64,
    pub distinct_all: i64,
    pub dup_uids: i64,
    pub worst_dup: i64,
    pub worst_dup_session: String,
    pub stuck_sessions: Vec<(String, String)>,
    pub missing_db: bool,
    /// The largest LIVE transcripts by estimated tokens: (session_id, est tokens, distinct msgs).
    pub largest: Vec<(String, i64, i64)>,
    /// Of those, the ones close enough to a 1M-token window to be worth compacting now.
    pub at_risk: Vec<(String, i64, i64)>,
}

/// Rough token estimate: ~4 characters per token. Only used to RANK sessions, never as a
/// hard number - the real limit depends on the model behind the session.
const CHARS_PER_TOKEN: f64 = 4.0;
/// A 1M-token window is the common ceiling; flag at 50% of it.
const AT_RISK_TOKENS: i64 = 500_000;

impl Health {
    /// Rows per logical message across the live set. 1.0 = healthy, 2.0 = every
    /// message is stored twice.
    pub fn live_ratio(&self) -> f64 {
        if self.distinct_live == 0 {
            0.0
        } else {
            self.live_rows as f64 / self.distinct_live as f64
        }
    }

    /// Archive bloat factor (all rows / distinct logical messages).
    pub fn bloat_ratio(&self) -> f64 {
        if self.distinct_all == 0 {
            0.0
        } else {
            self.total_rows as f64 / self.distinct_all as f64
        }
    }

    pub fn is_problematic(&self) -> bool {
        self.dup_uids > 0 || self.bloat_ratio() >= 4.0 || !self.stuck_sessions.is_empty()
    }
}

/// Audit one profile's state.db.
pub fn audit_profile(home: &Path, profile: &str) -> Health {
    let mut h = Health {
        profile: profile.to_string(),
        ..Default::default()
    };
    let db = env::state_db(home, profile);
    if !db.exists() {
        h.missing_db = true;
        return h;
    }
    h.db_bytes = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
    h.db_mb = h.db_bytes as f64 / 1_048_576.0;

    let conn = match Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ) {
        Ok(c) => c,
        Err(_) => {
            h.missing_db = true;
            return h;
        }
    };

    // live rows vs distinct logical messages
    if let Ok(mut st) = conn.prepare(
        "SELECT COUNT(*), COUNT(DISTINCT message_uid) FROM messages \
         WHERE active = 1 AND message_uid IS NOT NULL",
    ) {
        if let Ok(row) = st.query_row([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))) {
            h.live_rows = row.0;
            h.distinct_live = row.1;
        }
    }
    // all rows vs distinct logical messages (archive bloat)
    if let Ok(mut st) = conn.prepare(
        "SELECT COUNT(*), COUNT(DISTINCT message_uid) FROM messages WHERE message_uid IS NOT NULL",
    ) {
        if let Ok(row) = st.query_row([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))) {
            h.total_rows = row.0;
            h.distinct_all = row.1;
        }
    }
    // duplicate live rows per logical message
    if let Ok(mut st) = conn.prepare(
        "SELECT COUNT(*) FROM (SELECT message_uid FROM messages \
         WHERE active = 1 AND message_uid IS NOT NULL GROUP BY message_uid HAVING COUNT(*) > 1)",
    ) {
        if let Ok(n) = st.query_row([], |r| r.get::<_, i64>(0)) {
            h.dup_uids = n;
        }
    }
    // worst single offender
    if let Ok(mut st) = conn.prepare(
        "SELECT session_id, COUNT(*) c FROM messages \
         WHERE active = 1 AND message_uid IS NOT NULL \
         GROUP BY message_uid, session_id ORDER BY c DESC LIMIT 1",
    ) {
        if let Ok(row) = st.query_row([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))) {
            if row.1 > 1 {
                h.worst_dup = row.1;
                h.worst_dup_session = row.0;
            }
        }
    }
    // sessions with a recorded compression failure
    if let Ok(mut st) = conn.prepare(
        "SELECT id, COALESCE(compression_failure_error,'') FROM sessions \
         WHERE compression_failure_error IS NOT NULL AND compression_failure_error <> ''",
    ) {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        {
            for r in rows.flatten() {
                h.stuck_sessions.push(r);
            }
        }
    }
    // sessions whose LIVE transcript is closest to the context wall
    if let Ok(mut st) = conn.prepare(
        "SELECT session_id, CAST(SUM(LENGTH(COALESCE(content,'')))/? AS INTEGER) AS est, \
                COUNT(DISTINCT message_uid) AS msgs \
         FROM messages WHERE active = 1 GROUP BY session_id ORDER BY est DESC LIMIT 5",
    ) {
        if let Ok(rows) = st.query_map([CHARS_PER_TOKEN], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        }) {
            for (sid, est, msgs) in rows.flatten() {
                if h.largest.len() < 3 {
                    h.largest.push((sid.clone(), est, msgs));
                }
                if est >= AT_RISK_TOKENS {
                    h.at_risk.push((sid, est, msgs));
                }
            }
        }
    }
    h
}

/// Audit every profile that exists in this home.
pub fn audit(home: &Path) -> Vec<Health> {
    env::profiles(home)
        .iter()
        .map(|p| audit_profile(home, p))
        .collect()
}

/// Print the fleet context-health table. Returns true when problems were found.
pub fn report(home: &Path) -> Result<bool> {
    println!("== CONTEXT HEALTH ==");
    let rows = audit(home);
    let mut problems = false;
    println!(
        "  {:<10} {:>8} {:>10} {:>12} {:>8} {:>9}  worst",
        "profile", "db", "live/dup", "bloat", "dupUIDs", "stuck"
    );
    for h in &rows {
        if h.missing_db {
            println!("  {:<10} (no state.db)", h.profile);
            continue;
        }
        let worst = if h.worst_dup > 1 {
            format!(
                "{} x{} in {}",
                h.worst_dup,
                h.worst_dup,
                h.worst_dup_session.chars().take(24).collect::<String>()
            )
        } else {
            "-".to_string()
        };
        println!(
            "  {:<10} {:>7.0}MB {:>9.2}x {:>11.1}x {:>8} {:>9}  {}",
            h.profile,
            h.db_mb,
            h.live_ratio(),
            h.bloat_ratio(),
            h.dup_uids,
            h.stuck_sessions.len(),
            worst
        );
        for (sid, err) in &h.stuck_sessions {
            println!(
                "      stuck session {} : {}",
                sid.chars().take(28).collect::<String>(),
                err.chars().take(90).collect::<String>()
            );
        }
        for (sid, est, msgs) in &h.largest {
            let flag = if h.at_risk.iter().any(|(s, _, _)| s == sid) {
                "  AT RISK - compact it"
            } else {
                ""
            };
            println!(
                "      largest live  {} : ~{}k tokens over {} msgs{}",
                sid.chars().take(28).collect::<String>(),
                est / 1000,
                msgs,
                flag
            );
        }
        for (sid, est, msgs) in &h.at_risk {
            println!(
                "      AT RISK       {} : ~{}k live tokens over {} live messages (near a 1M window - compact it)",
                sid.chars().take(28).collect::<String>(),
                est / 1000,
                msgs
            );
        }
        if h.is_problematic() {
            problems = true;
        }
    }
    if problems {
        println!("  [FAIL] context health: duplicate live rows / archive bloat / stuck compression sessions");
    } else {
        println!("  [ok]   context health: no duplicate live rows, no bloat, no stuck sessions");
    }
    Ok(problems)
}

/// Run the context-health audit, optionally adding the fleet's log-noise report.
pub fn run_with(home: &Path, logs: bool) -> Result<()> {
    let problems = report(home)?;
    // Print the log report BEFORE exiting: `--logs` asked for it, and an exit
    // first meant the flag silently produced nothing whenever health had a problem
    // — which is exactly when the logs are most interesting.
    if logs {
        println!();
        let _ = crate::logsignals::report(home, 12);
    }
    if problems {
        std::process::exit(1);
    }
    Ok(())
}

/// A profile's READINESS: is it configured so it can actually work right now?
///
/// The provider-surface tripwire answers "did the config change?". This answers
/// "is this profile even wired up correctly" - the configured provider has a
/// usable credential, the fallback chain exists, the compression engine is the
/// built-in one, and no OAuth MCP server is left enabled to hang the CLI.
#[derive(Debug, Default)]
pub struct Readiness {
    pub profile: String,
    pub model_provider: String,
    pub model_default: String,
    pub base_url: String,
    pub key_env: String,
    pub key_present: bool,
    pub custom_provider: bool,
    pub fallbacks: usize,
    pub engine: String,
    pub mcp_enabled: Vec<String>,
    pub mcp_oauth_enabled: Vec<String>,
    pub live_dup_uids: i64,
    pub bloat: f64,
    pub stuck_sessions: usize,
    pub problems: Vec<String>,
}

/// Environment variable that usually holds the key for a provider.
fn key_env_for(provider: &str) -> String {
    let p = provider.to_ascii_uppercase().replace('-', "_");
    match p.as_str() {
        "OPENROUTER" | "OPENROUTER_VOT" => "OPENROUTER_API_KEY".into(),
        "NVIDIA" => "NVIDIA_API_KEY".into(),
        "DEEPSEEK" => "DEEPSEEK_API_KEY".into(),
        "OPENCODE_GO" => "OPENCODE_GO_API_KEY".into(),
        "OPENCODE_ZEN" => "OPENCODE_ZEN_API_KEY".into(),
        "OPENAI" => "OPENAI_API_KEY".into(),
        "TOGETHER" => "TOGETHER_API_KEY".into(),
        _ => format!("{}_API_KEY", p),
    }
}

/// Read `KEY=value` names out of a .env file (values are never returned).
fn env_keys(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.starts_with('#') || !l.contains('=') {
                return None;
            }
            Some(l.split('=').next().unwrap_or("").trim().to_string())
        })
        .collect()
}

/// Audit one profile's readiness.
pub fn readiness(home: &Path, profile: &str) -> Readiness {
    let mut r = Readiness {
        profile: profile.to_string(),
        ..Default::default()
    };
    let cfg = env::config_yaml(home, profile);
    let Ok(text) = std::fs::read_to_string(&cfg) else {
        r.problems.push("no config.yaml".into());
        return r;
    };
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
        r.problems.push("config.yaml does not parse".into());
        return r;
    };
    let get = |v: &serde_yaml::Value, path: &str| -> String {
        let mut cur = v;
        for seg in path.split('.') {
            match cur.get(seg) {
                Some(n) => cur = n,
                None => return String::new(),
            }
        }
        match cur {
            serde_yaml::Value::String(s) => s.clone(),
            other => serde_yaml::to_string(other)
                .unwrap_or_default()
                .trim()
                .trim_matches('"')
                .to_string(),
        }
    };
    r.model_provider = get(&doc, "model.provider");
    r.model_default = get(&doc, "model.default");
    r.base_url = get(&doc, "model.base_url");

    // the credential the configured provider actually uses
    let custom_key_env = get(&doc, &format!("providers.{}.api_key_env", r.model_provider));
    if !custom_key_env.is_empty() {
        r.custom_provider = true;
        r.key_env = custom_key_env;
    } else {
        r.key_env = key_env_for(&r.model_provider);
    }
    let env_path = env::profile_dir(home, profile).join(".env");
    let keys = env_keys(&env_path);
    r.key_present = keys.contains(&r.key_env);
    if r.model_provider.is_empty() {
        r.problems.push("no model.provider configured".into());
    } else if r.model_default.is_empty() {
        r.problems.push("no model.default configured".into());
    }
    if !r.model_provider.is_empty() && !r.key_present {
        // nous is an OAuth login, not an env key - do not call it missing
        if r.model_provider != "nous" {
            r.problems.push(format!(
                "provider '{}' has no credential in .env (expected {})",
                r.model_provider, r.key_env
            ));
        }
    }

    // fallback chain (top-level list, or model.fallback list)
    let fb_top = doc
        .get("fallback_providers")
        .and_then(|v| v.as_sequence())
        .map(|s| s.len())
        .unwrap_or(0);
    let fb_model = doc
        .get("model")
        .and_then(|m| m.get("fallback"))
        .and_then(|v| v.as_sequence())
        .map(|s| s.len())
        .unwrap_or(0);
    r.fallbacks = fb_top.max(fb_model);

    // compression engine
    r.engine = {
        let e = get(&doc, "context.engine");
        if e.is_empty() {
            "built-in".into()
        } else {
            e
        }
    };
    if r.engine != "built-in" {
        r.problems.push(format!(
            "custom compression engine '{}' bypasses the built-in summariser",
            r.engine
        ));
    }

    // MCP servers: enabled ones, and which are OAuth (the CLI-startup hang class)
    if let Some(map) = doc.get("mcp_servers").and_then(|v| v.as_mapping()) {
        for (name, spec) in map {
            let enabled = spec
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if enabled {
                let n = name.as_str().unwrap_or("?").to_string();
                r.mcp_enabled.push(n.clone());
                if spec.get("auth").and_then(|v| v.as_str()) == Some("oauth") {
                    r.mcp_oauth_enabled.push(n);
                }
            }
        }
    }

    // transcript health for this profile
    let db = env::state_db(home, profile);
    if db.exists() {
        if let Ok(conn) = Connection::open_with_flags(
            &db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ) {
            if let Ok(mut st) = conn.prepare(
                "SELECT COUNT(*) FROM (SELECT message_uid FROM messages WHERE active = 1 \
                 AND message_uid IS NOT NULL GROUP BY message_uid HAVING COUNT(*)>1)",
            ) {
                if let Ok(n) = st.query_row([], |r| r.get::<_, i64>(0)) {
                    r.live_dup_uids = n;
                }
            }
            if let Ok(mut st) = conn.prepare(
                "SELECT COUNT(*), COUNT(DISTINCT message_uid) FROM messages WHERE message_uid IS NOT NULL",
            ) {
                if let Ok((t, d)) = st.query_row([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))) {
                    if d > 0 {
                        r.bloat = t as f64 / d as f64;
                    }
                }
            }
            if let Ok(mut st) = conn.prepare(
                "SELECT COUNT(*) FROM sessions WHERE compression_failure_error IS NOT NULL \
                 AND compression_failure_error <> ''",
            ) {
                if let Ok(n) = st.query_row([], |r| r.get::<_, i64>(0)) {
                    r.stuck_sessions = n as usize;
                }
            }
        }
    }
    if r.live_dup_uids > 0 {
        r.problems.push(format!(
            "{} live message(s) stored twice - the prompt is inflated",
            r.live_dup_uids
        ));
    }
    r
}

/// Print one profile's readiness.
pub fn report_readiness(home: &Path, profile: &str) -> Result<bool> {
    let r = readiness(home, profile);
    println!("== PROFILE READINESS: {} ==", r.profile);
    println!(
        "  model           : {}/{}",
        r.model_provider, r.model_default
    );
    if !r.base_url.is_empty() {
        println!("  base_url        : {}", r.base_url);
    }
    println!(
        "  credential      : {}{}",
        r.key_env,
        if r.key_present {
            " (present)"
        } else if r.model_provider == "nous" {
            " (OAuth login - no env key expected)"
        } else {
            " (MISSING)"
        }
    );
    if r.custom_provider {
        println!("  provider        : custom provider defined in config");
    }
    println!("  fallbacks       : {}", r.fallbacks);
    if r.fallbacks == 0 {
        println!(
            "                   (no fallback chain - a rate-limited or dead primary ENDS the turn;"
        );
        println!("                    this is why default/polloriko went down on 402/429)");
    }
    println!("  compression eng : {}", r.engine);
    println!(
        "  mcp enabled     : {}",
        if r.mcp_enabled.is_empty() {
            "(none)".to_string()
        } else {
            r.mcp_enabled.join(", ")
        }
    );
    if !r.mcp_oauth_enabled.is_empty() {
        println!(
            "  mcp oauth       : {}  (CLI startup hang class on v0.22.2 - the gateway is fine, one-shot CLI runs are not)",
            r.mcp_oauth_enabled.join(", ")
        );
    }
    println!(
        "  transcript      : dup_uids={} bloat={:.1}x stuck_sessions={}",
        r.live_dup_uids, r.bloat, r.stuck_sessions
    );
    if r.problems.is_empty() {
        println!("  [ok]   profile looks ready");
        return Ok(false);
    }
    println!("  [FAIL] {} problem(s):", r.problems.len());
    for p in &r.problems {
        println!("      - {}", p);
    }
    Ok(true)
}

/// Size of a profile's state.db in bytes (used by backup to flag DBs that
/// Hermes' own 1 GB snapshot limit will skip).
pub fn db_size(home: &Path, profile: &str) -> u64 {
    let db = env::state_db(home, profile);
    std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0)
}

/// The context/compression engine a profile is configured to use, if it is not
/// the built-in one. A custom engine can bypass the built-in summariser - the
/// other half of the "context too long" incident, and invisible until now.
///
/// The key lives under `context:` (modern) or `compression:` (older layouts).
pub fn custom_compression_engine(home: &Path, profile: &str) -> Option<String> {
    let cfg = env::config_yaml(home, profile);
    let txt = std::fs::read_to_string(&cfg).ok()?;
    let mut section = "";
    for line in txt.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indented = line.starts_with(char::is_whitespace);
        if !indented {
            // a new top-level key ends the previous section
            section = trimmed.split(':').next().unwrap_or("").trim();
            continue;
        }
        if section != "context" && section != "compression" {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("engine:") {
            let v = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() && v != "built-in" && v != "default" {
                return Some(v);
            }
        }
    }
    None
}
