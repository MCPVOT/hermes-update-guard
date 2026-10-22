//! Log-signal signatures: what the fleet is actually logging, and whether an
//! UPDATE caused it.
//!
//! WHY
//!   The tripwires watch the provider surface and the transcript. Nobody was
//!   watching the LOGS, and that is where the real damage shows: a
//!   classic-PAT copilot probe logged ~5,900 warnings across four profiles in
//!   16 hours, and two OAuth MCP servers logged ~1,200 failures because a
//!   headless gateway cannot complete an interactive authorisation. Both were
//!   invisible to every existing check.
//!
//! WHAT IT DOES
//!   - `signatures()` groups every ERROR/WARNING/exception in the fleet's logs
//!     into stable signatures (so a repeated failure counts once, not per line).
//!   - `snapshot()` writes the counts to disk BEFORE an update, and
//!     `diff_against()` reports which signatures are NEW afterwards, so the
//!     verdict can attribute a regression to the update instead of guessing.
//!   - `report()` prints the fleet's top noise sources.
//!
//! Read-only. It never writes to the Hermes home except the snapshot file
//! (which lives under backups/, next to the other guard artefacts).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One grouped error/warning signature.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Signal {
    pub label: String,
    pub signature: String,
    pub count: i64,
    pub last_seen: String,
}

/// The fleet's log signature set: key = "<profile>|<signature>".
pub type Signals = BTreeMap<String, Signal>;

fn parse_ts(line: &str) -> Option<String> {
    // "2026-10-21 14:36:00,123" or "2026-10-21T14:36:00Z" at the start of a line.
    // Index 10 is the date/time separator: a SPACE or 'T', never ':'. Requiring ':'
    // there (as an earlier version did) meant no timestamp ever parsed, so every
    // signature reported an empty `last_seen`.
    let s: String = line.chars().take(19).collect();
    let c: Vec<char> = s.chars().collect();
    if c.len() < 19 {
        return None;
    }
    let ok = c[4] == '-'
        && c[7] == '-'
        && (c[10] == ' ' || c[10] == 'T')
        && c[13] == ':'
        && c[16] == ':'
        && c.iter()
            .all(|ch| ch.is_ascii_digit() || "-: T".contains(*ch));
    ok.then_some(s)
}

/// Drop a leading log timestamp (and its millisecond tail) so the same event
/// logged at different times collapses into one signature.
///
/// Without this, 93.5% of live signatures began with a literal timestamp: every
/// occurrence was unique, `count` was always 1, and "grouping" did not happen at
/// all — 23,805 distinct signatures with 99% singletons.
fn strip_leading_ts(line: &str) -> String {
    let Some(ts) = parse_ts(line) else {
        return line.to_string();
    };
    let mut rest = &line[ts.len()..];
    // ",123" / ".123" / "Z" trailing the seconds
    rest = rest.trim_start_matches('Z');
    if let Some(after) = rest.strip_prefix(',') {
        rest = after.trim_start_matches(|c: char| c.is_ascii_digit());
    } else if let Some(after) = rest.strip_prefix('.') {
        rest = after.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    rest.trim_start().to_string()
}

/// Normalise a line into a stable signature: the shape of the problem, not its
/// ids/timestamps/paths.
fn normalise(line: &str) -> String {
    let mut s = strip_leading_ts(line.trim());
    // hex ids
    let mut out = String::new();
    let mut word = String::new();
    let flush = |w: &str, out: &mut String| {
        let wl = w.to_ascii_lowercase();
        if wl.len() >= 8 && wl.chars().all(|c| c.is_ascii_hexdigit()) {
            out.push_str("<hex>");
        } else if w.len() >= 3 && w.chars().all(|c| c.is_ascii_digit()) {
            out.push_str("<n>");
        } else {
            out.push_str(w);
        }
    };
    for ch in s.chars() {
        if ch.is_whitespace() {
            flush(&word, &mut out);
            out.push(' ');
            word.clear();
        } else {
            word.push(ch);
        }
    }
    flush(&word, &mut out);
    s = out.split_whitespace().collect::<Vec<_>>().join(" ");
    // truncate by CHARS: String::truncate panics on a non-char boundary, and a log
    // line is arbitrary UTF-8 (a CJK or emoji line made the guard crash).
    if s.chars().count() > 140 {
        s = s.chars().take(140).collect();
    }
    s
}

/// Is this line a problem? Matched on whole tokens so `ERROR:`, `[ERROR]` and
/// `level=ERROR` all count — requiring the literal " ERROR " (spaces both sides)
/// missed every structured-log format.
fn is_errorish(line: &str) -> bool {
    if line.starts_with("Traceback (most recent call last)") {
        return true;
    }
    let up = line.to_ascii_uppercase();
    up.split(|c: char| !c.is_ascii_alphanumeric()).any(|tok| {
        matches!(
            tok,
            "ERROR" | "CRITICAL" | "WARNING" | "WARN" | "EXCEPTION" | "FATAL"
        )
    }) || line.contains("Error:")
        || line.contains("Exception")
}

/// Scan one log file, folding it into the signal map.
fn scan(path: &Path, label: &str, out: &mut Signals) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut in_tb = false;
    let mut last_ts = String::new();
    for line in text.lines() {
        if let Some(t) = parse_ts(line) {
            last_ts = t;
        }
        if line.starts_with("Traceback (most recent call last)") {
            in_tb = true;
            continue;
        }
        if in_tb {
            // A traceback is ONE event. Frames are indented and carry no identity;
            // the first non-indented line IS the exception. Counting the `raise`
            // frame as well produced three signatures for one failure.
            if line.starts_with(char::is_whitespace) {
                continue;
            }
            if line.trim().is_empty() {
                in_tb = false;
                continue;
            }
            let sig = normalise(line.trim());
            bump(out, label, &format!("EXC {sig}"), &last_ts);
            in_tb = false;
            continue; // consumed: do not also count it as a plain ERROR line
        }
        if is_errorish(line) {
            let sig = normalise(line.trim());
            bump(out, label, &sig, &last_ts);
        }
    }
}

fn bump(out: &mut Signals, label: &str, sig: &str, ts: &str) {
    let key = format!("{}|{}", label, sig);
    let e = out.entry(key).or_insert_with(|| Signal {
        label: label.to_string(),
        signature: sig.to_string(),
        count: 0,
        last_seen: String::new(),
    });
    e.count += 1;
    if !ts.is_empty() && ts > e.last_seen.as_str() {
        e.last_seen = ts.to_string();
    }
}

/// Is this file one of the fleet's logs, including rotated ones?
///
/// Only `extension() == "log"` was read, so ~35 MB of `*.log.1/.2/.3` was
/// invisible — which also meant a rotated log could not produce a "new" signal.
fn is_log(p: &Path) -> bool {
    if p.extension().map(|x| x == "log").unwrap_or(false) {
        return true;
    }
    p.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.contains(".log."))
        .unwrap_or(false)
}

/// Label for a log file: `agent.log` → `agent`, `agent.log.2` stays `agent.log.2`.
/// Stable for the plain file, distinct for a rotation, so an upgrade does not
/// relabel every existing signal as "new".
fn log_label(p: &Path) -> String {
    let n = p
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    match n.strip_suffix(".log") {
        Some(base) => base.to_string(),
        None => n,
    }
}

/// Every log the guard watches: the main logs plus each profile's logs.
fn log_files(home: &Path) -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    let logs = home.join("logs");
    if let Ok(rd) = std::fs::read_dir(&logs) {
        for e in rd.flatten() {
            let p = e.path();
            if is_log(&p) {
                files.push((p.clone(), format!("main/{}", log_label(&p))));
            }
        }
    }
    let profiles = home.join("profiles");
    if let Ok(rd) = std::fs::read_dir(&profiles) {
        for e in rd.flatten() {
            let dir = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            let plogs = dir.join("logs");
            if let Ok(rd2) = std::fs::read_dir(&plogs) {
                for e2 in rd2.flatten() {
                    let p = e2.path();
                    if is_log(&p) {
                        files.push((p.clone(), format!("{}/{}", name, log_label(&p))));
                    }
                }
            }
        }
    }
    files
}

/// Build the fleet's signal set.
pub fn signals(home: &Path) -> Signals {
    let mut out = Signals::new();
    for (path, label) in log_files(home) {
        scan(&path, &label, &mut out);
    }
    out
}

/// Where the pre-update snapshot lives.
pub fn snapshot_path(home: &Path) -> PathBuf {
    home.join("backups").join("guard-logsignals.json")
}

/// Write the current signal counts (call BEFORE an update).
pub fn snapshot(home: &Path) -> Result<PathBuf> {
    let sigs = signals(home);
    let p = snapshot_path(home);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, serde_json::to_string_pretty(&sigs)?)?;
    Ok(p)
}

/// Read the stored snapshot, if any.
pub fn load_snapshot(home: &Path) -> Option<Signals> {
    let p = snapshot_path(home);
    let text = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&text).ok()
}

/// Signatures that appeared (or grew) since the snapshot.
pub fn diff_against(home: &Path, before: &Signals) -> Vec<Signal> {
    let now = signals(home);
    let mut out = Vec::new();
    for (key, s) in &now {
        let prev = before.get(key).map(|p| p.count).unwrap_or(0);
        if s.count > prev {
            let mut d = s.clone();
            d.count = s.count - prev;
            out.push(d);
        }
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.count));
    out
}

/// One signature across every log file that reported it.
#[derive(Debug, Clone)]
pub struct Merged {
    pub signature: String,
    pub count: i64,
    pub last_seen: String,
    pub labels: Vec<String>,
}

/// Collapse per-file rows that share a signature.
///
/// The key is `"<profile>/<stream>|<signature>"`, so one OAuth failure logged to
/// `agent`, `errors` and `gateway` was reported three times and read as three
/// problems. This is display-only: the stored snapshot keeps the per-file keys.
fn merge(signals: impl IntoIterator<Item = Signal>) -> Vec<Merged> {
    let mut map: BTreeMap<String, Merged> = BTreeMap::new();
    for s in signals {
        let e = map.entry(s.signature.clone()).or_insert_with(|| Merged {
            signature: s.signature.clone(),
            count: 0,
            last_seen: String::new(),
            labels: Vec::new(),
        });
        e.count += s.count;
        if s.last_seen > e.last_seen {
            e.last_seen = s.last_seen.clone();
        }
        if !e.labels.iter().any(|l| l == &s.label) {
            e.labels.push(s.label.clone());
        }
    }
    let mut v: Vec<Merged> = map.into_values().collect();
    v.sort_by_key(|b| std::cmp::Reverse(b.count));
    for m in &mut v {
        m.labels.sort();
    }
    v
}

/// Print the fleet's top log-noise sources.
pub fn report(home: &Path, top: usize) -> Result<()> {
    let sigs = signals(home);
    let merged = merge(sigs.into_values());
    println!("== LOG SIGNALS (fleet) ==");
    println!(
        "  {} distinct signatures across the fleet's logs ({} per-file rows)",
        merged.len(),
        merged.iter().map(|m| m.labels.len()).sum::<usize>()
    );
    for s in merged.iter().take(top) {
        println!(
            "  [{} x{} last {}]",
            s.labels.join(", "),
            s.count,
            s.last_seen
        );
        println!("       {}", s.signature);
    }
    Ok(())
}

/// Compare against the stored snapshot and report NEW signals.
pub fn report_diff(home: &Path) -> Result<bool> {
    let Some(before) = load_snapshot(home) else {
        println!("  no log-signal snapshot on disk - nothing to compare against");
        return Ok(false);
    };
    let new = diff_against(home, &before);
    if new.is_empty() {
        println!("  no new log signatures since the pre-update snapshot");
        return Ok(false);
    }
    println!("== LOG SIGNATURES INTRODUCED SINCE THE PRE-UPDATE SNAPSHOT ==");
    for s in merge(new) {
        println!(
            "  [{} +{} last {}]",
            s.labels.join(", "),
            s.count,
            s.last_seen
        );
        println!("       {}", s.signature);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One event written to three log files was reported as three problems.
    #[test]
    fn merge_collapses_one_signature_from_several_logs() {
        let input = vec![
            Signal {
                label: "ofcrm/agent".into(),
                signature: "OAuthNonInteractiveError".into(),
                count: 6,
                last_seen: "2026-10-21 20:45:50".into(),
            },
            Signal {
                label: "ofcrm/errors".into(),
                signature: "OAuthNonInteractiveError".into(),
                count: 6,
                last_seen: "2026-10-21 20:45:50".into(),
            },
            Signal {
                label: "main/errors".into(),
                signature: "gateway exited UNCLEANLY".into(),
                count: 1,
                last_seen: "2026-10-21 20:44:13".into(),
            },
        ];
        let m = merge(input);
        assert_eq!(m.len(), 2, "two distinct signatures, not three rows");
        assert_eq!(m[0].count, 12, "counts are summed across files");
        assert_eq!(m[0].labels, vec!["ofcrm/agent", "ofcrm/errors"]);
        assert_eq!(m[1].count, 1);
    }

    /// The bug this replaced: index 10 is a space or 'T', never ':', so no
    /// timestamp ever parsed and every signature reported an empty last_seen.
    #[test]
    fn parse_ts_accepts_space_and_t_and_rejects_junk() {
        assert_eq!(
            parse_ts("2026-10-21 14:36:00,123 ERROR boom"),
            Some("2026-10-21 14:36:00".to_string())
        );
        assert_eq!(
            parse_ts("2026-10-21T14:36:00Z ERROR boom"),
            Some("2026-10-21T14:36:00".to_string())
        );
        assert_eq!(parse_ts("no timestamp here at all"), None);
        assert_eq!(parse_ts("2026/10/21 14:36:00"), None);
    }

    /// A line starting with multi-byte UTF-8 must not panic. The previous version
    /// byte-sliced a char-built String (`s[4..5]`), which panics off a boundary.
    #[test]
    fn non_ascii_lines_do_not_panic() {
        let _ = parse_ts("日本語のログ ERROR 発生");
        let long = "错误".repeat(200);
        let sig = normalise(&long);
        assert_eq!(sig.chars().count(), 140, "truncate must count chars");
        let _ = parse_ts("🚨🚨🚨🚨🚨🚨🚨🚨🚨🚨 ERROR");
    }

    /// Structured logs write `ERROR:`, `[ERROR]`, `level=ERROR` — the old check
    /// required the literal " ERROR " and missed all of them.
    #[test]
    fn is_errorish_catches_structured_formats() {
        assert!(is_errorish("2026-10-21 14:36:00,123 ERROR: nope"));
        assert!(is_errorish("2026-10-21 14:36:00 [ERROR] nope"));
        assert!(is_errorish("ts=1 level=ERROR msg=x"));
        assert!(is_errorish("Traceback (most recent call last)"));
        assert!(!is_errorish("2026-10-21 14:36:00,123 INFO all good"));
        assert!(
            !is_errorish("no errors were reported"),
            "plural is not ERROR"
        );
    }

    #[test]
    fn normalise_folds_timestamps() {
        // the defect: 93.5% of signatures began with a literal timestamp
        let a = normalise("2026-10-21 14:36:00,123 ERROR: upstream 500");
        let b = normalise("2026-10-21 15:02:11,999 ERROR: upstream 500");
        assert_eq!(a, b);
        assert!(!a.contains("2026"), "timestamp must be stripped: {a}");
        assert!(a.starts_with("ERROR"), "got: {a}");
        // and the T-form / Z-form too
        let c = normalise("2026-10-21T14:36:00Z ERROR: upstream 500");
        assert_eq!(a, c);
    }

    /// Same shape, different ids/numbers => one signature, not one per line.
    #[test]
    fn normalise_folds_ids_and_numbers() {
        let a = normalise("ERROR session 12a4f9bc failed after 333 msgs");
        let b = normalise("ERROR session deadbeef failed after 777 msgs");
        assert_eq!(a, b);
        assert!(a.contains("<hex>"));
        assert!(a.contains("<n>"));
    }
}
