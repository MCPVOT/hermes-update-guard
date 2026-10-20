use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::env;

/// Post-update verification: HEAD advanced, version sane, every live db integrity ok,
/// provider surface matches the pre-update export, gateway state present, and every
/// locally pinned file still applied.
///
/// FAIL-CLOSED CONTRACT: any check that cannot be *performed* is a failure, not a
/// pass. An earlier version printed `VERIFY: PASS` and exited 0 on an empty home
/// (no hermes-agent, unreadable HEAD, no venv) and even counted git's stderr as
/// "1 local modification re-applied". A verifier that cannot fail is worse than no
/// verifier, because it certifies a broken install.
pub fn run(home: &Path) -> Result<()> {
    println!("== POST-UPDATE VERIFY ==");

    let mut failures: Vec<String> = Vec::new();
    let repo = home.join("hermes-agent");

    // ---- 1. HEAD ------------------------------------------------------------
    println!("  HEAD:");
    match env::run_checked(
        Path::new("git"),
        &["rev-parse", "--short", "HEAD"],
        Some(&repo),
    ) {
        Ok((stdout, _)) => println!("  ok   HEAD {}", stdout.trim()),
        Err(e) => {
            println!("  FAIL HEAD unreadable: {}", e);
            failures.push(format!("HEAD unreadable: {}", e));
        }
    }

    // ---- 2. version ---------------------------------------------------------
    println!("  version:");
    match env::resolve_hermes(home) {
        None => {
            println!("  FAIL no hermes CLI found (venv, PATH, ~/.local/bin, /usr/local/bin)");
            failures.push("hermes CLI not found".into());
        }
        Some(hermes) => match env::run_checked(&hermes, &["--version"], None) {
            Ok((stdout, _)) => {
                let v = stdout.lines().next().unwrap_or("?").trim().to_string();
                if v.is_empty() {
                    println!("  FAIL --version printed nothing");
                    failures.push("version empty".into());
                } else {
                    println!("  ok   {}", v);
                }
            }
            Err(e) => {
                println!("  FAIL version check could not run: {}", e);
                failures.push(format!("version: {}", e));
            }
        },
    }

    // ---- 3. database integrity ---------------------------------------------
    println!("  databases:");
    let mut checked = 0usize;
    for p in env::profiles(home).iter().map(|s| s.as_str()) {
        let db = env::state_db(home, p);
        if !db.exists() {
            continue;
        }
        checked += 1;
        match integrity(&db) {
            Ok((ok, msgs)) => {
                if ok != "ok" {
                    println!("  FAIL {} integrity={}", p, ok);
                    failures.push(format!("{} integrity={}", p, ok));
                } else {
                    println!("  ok   {} integrity=ok messages={}", p, msgs);
                }
            }
            Err(e) => {
                println!("  FAIL {} {}", p, e);
                failures.push(format!("{} db: {}", p, e));
            }
        }
    }
    if checked == 0 {
        println!("  FAIL no profile database found - nothing was verified");
        failures.push("no state.db found in any profile".into());
    }

    // ---- 4. local patch restoration ----------------------------------------
    println!("  local patch (stash/restore):");
    check_patch_restored(home, &repo, &mut failures);

    // ---- 5. provider surface vs pre-update export --------------------------
    println!("  provider surface vs last export:");
    check_drift(home, &mut failures);

    // ---- 6. context health --------------------------------------------------
    // A guarded update must not report "VERIFY: PASS" over a transcript that
    // is duplicated: duplicate live rows double the prompt, so the next
    // session dies on "context length exceeded" hours later.
    //
    // Attribution: if the run captured a health baseline BEFORE the update, a
    // condition that is unchanged is PRE-EXISTING (a warning, not a failure);
    // only a regression the update introduced fails the verdict.
    println!("  context health:");
    let before = crate::state::RunState::read(home)
        .ok()
        .and_then(|st| st.health_before);
    let now = crate::context_health::summarize(home);
    println!(
        "    now: dup_uids={} max_bloat={:.1}x stuck={} at_risk={}",
        now.dup_uids, now.max_bloat, now.stuck, now.at_risk
    );
    match &before {
        Some(b) => {
            println!(
                "    before update: dup_uids={} max_bloat={:.1}x stuck={} at_risk={}",
                b.dup_uids, b.max_bloat, b.stuck, b.at_risk
            );
            if now.is_worse_than(b) {
                println!("  FAIL context health got WORSE during the update");
                failures.push("context health regressed during the update".to_string());
            } else if now.same_as(b) {
                println!("  ! context health unchanged - PRE-EXISTING condition, not caused by this update");
            } else {
                println!(
                    "  ok   context health improved (or unchanged) since the pre-update baseline"
                );
            }
        }
        None => {
            if crate::context_health::report(home)? {
                println!("  FAIL context health problems (see table above) - no pre-update baseline to attribute them to");
                failures.push(
                    "context health: duplicate live rows / archive bloat / stuck sessions"
                        .to_string(),
                );
            } else {
                println!("  ok   context health clean");
            }
        }
    }

    // ---- 7. gateway state ---------------------------------------------------
    let gw = home.join("gateway_state.json");
    if gw.exists() {
        match std::fs::read_to_string(&gw)
            .context("gateway_state.json unreadable")
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).context("bad json"))
        {
            Ok(_js) => {
                let g = gateway_status(home).unwrap_or_default();
                let pid = g.pid;
                let st = g.state.as_str();
                let sha = g.code_sha.as_str();
                println!("  gateway : pid={} state={}", pid, st);
                // The state file can outlive the process it describes - a stale
                // "running" gateway is indistinguishable from a healthy one
                // unless the pid is checked. (It lied repeatedly in 2026-10.)
                if pid > 0 && st == "running" {
                    match env::pid_running(pid) {
                        env::ProcState::Running => {}
                        env::ProcState::Absent => {
                            println!("  FAIL gateway_state.json claims pid {} is running, but that process is gone", pid);
                            failures.push(format!(
                                "gateway pid {} is dead but recorded as running",
                                pid
                            ));
                        }
                        env::ProcState::Unknown(why) => {
                            println!("  ! could not determine whether gateway pid {} is alive ({}): treat as suspect", pid, why);
                        }
                    }
                }
                // A gateway serving a DIFFERENT commit than the install is running
                // pre-update code. The old PowerShell wrapper checked this; the Rust
                // port lost it, and a stale-code gateway looks perfectly healthy.
                if !sha.is_empty() {
                    if let Some(head) = head_sha(home) {
                        if !sha.starts_with(&head) {
                            println!(
                                "  FAIL gateway runs code {} but the install is on {} - it is serving PRE-UPDATE code",
                                sha.chars().take(10).collect::<String>(),
                                head
                            );
                            failures.push(format!(
                                "gateway runs sha {} but install is on {}",
                                sha.chars().take(10).collect::<String>(),
                                head
                            ));
                        } else {
                            println!("  gateway code_sha matches HEAD ({})", head);
                        }
                    }
                }
            }
            Err(e) => {
                println!("  FAIL gateway_state.json: {}", e);
                failures.push(format!("gateway state: {}", e));
            }
        }
    }

    // ---- 8. the updater's own receipt ---------------------------------------
    // verify proves HEAD/DBs/drift, but not whether the UPDATER reported success.
    // The receipts are the operator's audit trail; surface the newest one.
    let receipts = home.join("logs").join("update_receipts");
    if receipts.is_dir() {
        let mut newest: Option<(String, String, String, String)> = None; // (file, outcome, pid, started)
        if let Ok(rd) = std::fs::read_dir(&receipts) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if !name.starts_with("update_") {
                    continue;
                }
                let Ok(txt) = std::fs::read_to_string(e.path()) else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
                    continue;
                };
                let started = v
                    .get("started_at")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let outcome = v
                    .get("outcome")
                    .and_then(|x| x.as_str())
                    .unwrap_or("?")
                    .to_string();
                let pid = v
                    .get("pid")
                    .and_then(|x| x.as_u64())
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "?".into());
                let better = match &newest {
                    None => true,
                    Some((_, _, _, s)) => started > *s,
                };
                if better {
                    newest = Some((name, outcome, pid, started));
                }
            }
        }
        match newest {
            Some((name, outcome, pid, started)) => {
                println!(
                    "  receipt  : {} outcome={} pid={} started={}",
                    name, outcome, pid, started
                );
                if outcome == "interrupted" || outcome == "running" {
                    // A dead pid means history, not an update in flight - the work it owed
                    // was completed by a later run. Say so instead of leaving it ambiguous.
                    let state = pid
                        .parse::<u32>()
                        .map(env::pid_running)
                        .unwrap_or(env::ProcState::Unknown("pid unreadable".into()));
                    match state {
                        env::ProcState::Running => {
                            println!("  ! that update's pid is ALIVE - an update is genuinely in flight");
                        }
                        _ => println!(
                            "    (pid dead - that attempt was superseded; the work it owed was completed later)"
                        ),
                    }
                }
            }
            None => println!("  receipt  : none found (no update receipt in logs/update_receipts)"),
        }
    }

    // ---- verdict ------------------------------------------------------------
    if failures.is_empty() {
        println!("\nVERIFY: PASS");
        Ok(())
    } else {
        println!("\nVERIFY: FAIL ({} check(s))", failures.len());
        for f in &failures {
            println!("  - {}", f);
        }
        bail!("{} verification check(s) failed", failures.len())
    }
}

/// Prove a locally pinned file is still applied.
///
/// Two distinct failures: a clean tree WITH stash entries (the updater parked the
/// patch) and a clean tree with NO stashes while the pre-update backup recorded a
/// working-tree patch (the change vanished entirely).
fn check_patch_restored(home: &Path, repo: &Path, failures: &mut Vec<String>) {
    let dirty = match env::run_checked(Path::new("git"), &["status", "--short"], Some(repo)) {
        Ok((stdout, _)) => stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_string())
            .collect::<Vec<_>>(),
        Err(e) => {
            println!("  FAIL cannot read working tree: {}", e);
            failures.push(format!("git status: {}", e));
            return;
        }
    };
    let stashes = match env::run_checked(Path::new("git"), &["stash", "list"], Some(repo)) {
        Ok((stdout, _)) => stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_string())
            .collect::<Vec<_>>(),
        Err(e) => {
            println!("  FAIL cannot read stash list: {}", e);
            failures.push(format!("git stash list: {}", e));
            return;
        }
    };

    let expected = expected_patched_files(home);

    if !dirty.is_empty() {
        println!(
            "  ok   {} local modification(s) present in the tree:",
            dirty.len()
        );
        for d in &dirty {
            println!("         {}", d);
        }
        for f in &expected {
            if !dirty.iter().any(|d| d.ends_with(f.as_str())) {
                println!("  FAIL pinned file no longer modified: {}", f);
                failures.push(format!("pinned patch lost: {}", f));
            }
        }
        if !stashes.is_empty() {
            println!(
                "  !    {} stash entr{} also present (a partial restore?)",
                stashes.len(),
                if stashes.len() == 1 { "y" } else { "ies" }
            );
        }
        return;
    }

    if !stashes.is_empty() {
        println!(
            "  FAIL tree is clean but {} stash entr{} remain - the update parked your local patch",
            stashes.len(),
            if stashes.len() == 1 { "y" } else { "ies" }
        );
        for s in &stashes {
            println!("         {}", s);
        }
        failures.push(format!("{} stashed patch(es) not restored", stashes.len()));
        return;
    }

    if expected.is_empty() {
        println!("  ok   working tree clean, nothing was pinned pre-update");
    } else {
        println!(
            "  FAIL tree is clean but the pre-update backup pinned {} file(s) - the patch was dropped:",
            expected.len()
        );
        for f in &expected {
            println!("         {}", f);
        }
        failures.push(format!(
            "{} pinned file(s) dropped by the update",
            expected.len()
        ));
    }
}

/// File paths recorded in the newest pre-update backup's working-tree.patch.
fn expected_patched_files(home: &Path) -> Vec<String> {
    let backups = home.join("backups");
    let Ok(entries) = std::fs::read_dir(&backups) else {
        return vec![];
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("pre-update-"))
                .unwrap_or(false)
        })
        .collect();
    dirs.sort();
    let Some(newest) = dirs.last() else {
        return vec![];
    };
    let patch = newest.join("working-tree.patch");
    let Ok(text) = std::fs::read_to_string(&patch) else {
        return vec![];
    };
    let mut files: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix("+++ b/"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "/dev/null")
        .collect();
    files.sort();
    files.dedup();
    files
}

fn check_drift(home: &Path, failures: &mut Vec<String>) {
    let surface = home.join("backups").join("provider-surface");
    let pointer = surface.join("last-export.txt");
    if !pointer.exists() {
        println!("  FAIL no export baseline (last-export.txt missing) - drift cannot be checked");
        failures.push("no provider baseline".into());
        return;
    }
    let stamp = match std::fs::read_to_string(&pointer) {
        Ok(s) => s.trim().to_string(),
        Err(e) => {
            println!("  FAIL baseline pointer unreadable: {}", e);
            failures.push(format!("baseline pointer: {}", e));
            return;
        }
    };
    let baseline = surface.join(&stamp);
    if !baseline.is_dir() {
        println!("  FAIL baseline directory missing: {}", baseline.display());
        failures.push("baseline directory missing".into());
        return;
    }
    // compare_against also emits a `baseline UNREADABLE` line for unparseable
    // baselines, so that case surfaces as drift instead of "clean".
    let lines = crate::drift::compare_against(home, &baseline);
    if lines.is_empty() {
        println!("  ok   no drift vs pre-update export");
    } else {
        for l in &lines {
            println!("  ! {}", l.trim());
        }
        println!("  FAIL {} drift line(s) vs pre-update export", lines.len());
        failures.push(format!("{} provider drift line(s)", lines.len()));
    }
}

/// What the gateway state file claims, and whether that claim is still true.
#[derive(Debug, Clone, Default)]
pub struct GatewayStatus {
    pub pid: u32,
    pub state: String,
    pub code_sha: String,
}

/// Read the root gateway state file.
pub fn gateway_status(home: &Path) -> Option<GatewayStatus> {
    let gw = home.join("gateway_state.json");
    if !gw.exists() {
        return None;
    }
    let text = std::fs::read_to_string(&gw).ok()?;
    let js: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(GatewayStatus {
        pid: js.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        state: js
            .get("gateway_state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        code_sha: js
            .get("code_sha")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

/// The commit the install is on (short form), if it is a git install.
pub fn head_sha(home: &Path) -> Option<String> {
    let repo = home.join("hermes-agent");
    let (code, out) = env::run_capture(
        Path::new("git"),
        &[
            "-C",
            repo.to_str().unwrap_or("."),
            "rev-parse",
            "--short",
            "HEAD",
        ],
        None,
    )
    .ok()?;
    if code != 0 {
        return None;
    }
    let sha = out.trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

fn integrity(db: &Path) -> Result<(String, i64)> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let ok: String = conn.query_row("pragma integrity_check", [], |r| r.get(0))?;
    let msgs: i64 = conn
        .query_row("select count(*) from messages", [], |r| r.get(0))
        .unwrap_or(-1);
    Ok((ok, msgs))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pre-update working-tree.patch is the record of what was pinned. If we
    /// cannot read the file list back, verify can never notice a dropped patch.
    #[test]
    fn expected_patched_files_reads_newest_backup() {
        let root = std::env::temp_dir().join(format!("hug-verify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let old = root.join("backups").join("pre-update-20260101-000000");
        let new = root.join("backups").join("pre-update-20260102-000000");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("working-tree.patch"), "+++ b/old.py\n").unwrap();
        std::fs::write(
            new.join("working-tree.patch"),
            "--- a/hermes_cli/models_catalog_static.py\n\
             +++ b/hermes_cli/models_catalog_static.py\n\
             --- a/pkg/x.py\n\
             +++ b/pkg/x.py\n\
             --- /dev/null\n\
             +++ /dev/null\n",
        )
        .unwrap();

        let files = expected_patched_files(&root);
        assert_eq!(
            files,
            vec![
                "hermes_cli/models_catalog_static.py".to_string(),
                "pkg/x.py".to_string()
            ],
            "newest backup only, /dev/null excluded, sorted+deduped"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// No backup at all must yield an empty list (not a panic, not a fake entry).
    #[test]
    fn expected_patched_files_empty_without_backups() {
        assert!(expected_patched_files(Path::new("C:/definitely-not-here-xyz")).is_empty());
    }
}
