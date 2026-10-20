use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::env;

/// Read-only preflight: surface every condition that decides an update.
pub fn run(home: &Path) -> Result<()> {
    crate::vibe::print_compact(env!("CARGO_PKG_VERSION"));
    println!("== HERMES UPDATE PREFLIGHT ==");
    println!("home: {}", home.display());
    // Which binary is answering matters: a stale `~/.cargo/bin` copy reports
    // features the repo has removed and lacks the ones it added.
    println!(
        "guard: v{} at {}",
        env!("CARGO_PKG_VERSION"),
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "(unknown path)".into())
    );

    // version + update-channel state
    let hermes = env::venv_hermes(home);
    let (vcode, vout) = env::run_capture(&hermes, &["--version"], None)
        .unwrap_or((-1, "cannot run hermes --version".into()));
    println!("\n[version] (exit {})", vcode);
    for l in vout.lines().take(4) {
        println!("  {}", l);
    }

    let (ccode, cout) = env::run_capture(&hermes, &["update", "--check"], None)
        .unwrap_or((-1, "cannot run hermes update --check".into()));
    println!("\n[update --check] (exit {})", ccode);
    for l in cout.trim().lines().take(4) {
        println!("  {}", l);
    }
    // What will the update actually install? "Up to date" is ambiguous: the
    // channel decides the target, and the repo can still be N commits behind
    // origin/main without that mattering (stable tracks releases, not main).
    let target = cout
        .lines()
        .find(|l| l.contains("Selected release available") || l.contains("Update available"))
        .map(|l| l.trim().to_string());
    match target {
        Some(t) => println!("  TARGET : {}", t),
        None => {
            if cout.contains("Up to date") || cout.contains("No newer release") {
                println!(
                    "  TARGET : none - this update would be a NO-OP (already on the newest release)"
                );
            }
        }
    }

    // git state
    let repo = home.join("hermes-agent");
    if repo.join(".git").exists() {
        println!("[git @ {}]", repo.display());
        let (_, head) = env::run_capture(
            Path::new("git"),
            &["-C", repo.to_str().unwrap(), "rev-parse", "--short", "HEAD"],
            None,
        )
        .unwrap_or((-1, "?".into()));
        let (_, dirty) = env::run_capture(
            Path::new("git"),
            &["-C", repo.to_str().unwrap(), "status", "--short"],
            None,
        )
        .unwrap_or((-1, String::new()));
        // A detached HEAD has no upstream, and `rev-list HEAD..@{u}` then prints
        // "fatal: HEAD does not point to a branch" on stderr — which run_capture
        // merges into stdout, so the old code printed the fatal text as the count.
        // Ask for the branch first, and never let an error string become a number.
        let (_, branch_raw) = env::run_capture(
            Path::new("git"),
            &["-C", repo.to_str().unwrap(), "rev-parse", "--abbrev-ref", "HEAD"],
            None,
        )
        .unwrap_or((-1, "?".into()));
        let branch = branch_raw.trim().to_string();
        let behind = if branch == "HEAD" || branch.is_empty() {
            "n/a (detached HEAD, no upstream to count against)".to_string()
        } else {
            match env::run_checked(
                Path::new("git"),
                &["rev-list", "--count", "HEAD..@{u}"],
                Some(&repo),
            ) {
                Ok((stdout, _)) => format!("{} commits", stdout.trim()),
                Err(_) => "n/a (no upstream configured)".to_string(),
            }
        };
        let (_, stashes) = env::run_capture(
            Path::new("git"),
            &["-C", repo.to_str().unwrap(), "stash", "list"],
            None,
        )
        .unwrap_or((-1, String::new()));
        println!("  HEAD     : {}", head.trim());
        println!(
            "  branch   : {}",
            if branch == "HEAD" { "(detached)" } else { branch.as_str() }
        );
        println!("  behind   : {}", behind);
        println!("  stashes  : {}", stashes.lines().count());
        let dirty_lines: Vec<&str> = dirty.lines().collect();
        if dirty_lines.is_empty() {
            println!("  dirty    : (clean)");
        } else {
            println!(
                "  dirty    : {} file(s) — WILL be stashed by updater",
                dirty_lines.len()
            );
            for d in &dirty_lines {
                println!("             {}", d);
            }
        }
    } else {
        println!("\n[git] no .git at {} (not a git install?)", repo.display());
    }

    // venv holders + update plan
    println!("\n[venv holders]");
    let (holders, vexit, parsed) = env::venv_holders_probe(home);
    if !parsed {
        // hermes answered with prose, not the documented JSON list: that is what
        // it does while an update is already in flight. Never call this "free".
        for h in &holders {
            println!("  ! {}", h);
        }
        println!("  ! venv state UNKNOWN — hermes could not answer the probe; treat as HELD");
    } else if holders.is_empty() {
        println!("  (none) — probe exit {} (0 = venv free)", vexit);
    } else {
        for h in &holders {
            println!("  {}", h);
        }
        println!(
            "  probe exit {} (3 = holders present) — the update must wait for these",
            vexit
        );
    }

    // stale locks / interrupted-update markers
    println!("\n[locks / interrupted markers]");
    let mut found = false;
    for marker in [
        ".hermes-update-in-progress",
        ".hermes-update-in-progress.lock",
    ] {
        let p = home.join(marker);
        if p.exists() {
            println!("  ! {} present — previous update did not finish", marker);
            found = true;
        }
    }
    let qdir = home.join("backups").join("update-state-quarantine");
    if qdir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&qdir) {
            let mut dirs: Vec<_> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            dirs.sort();
            let detail = if dirs.is_empty() {
                String::new()
            } else {
                format!(" ({})", dirs.join(", "))
            };
            println!("  quarantine dir has {}{}", dirs.len(), detail);
        }
    }
    if !found {
        println!("  (no stale update markers)");
    }

    // interrupted-update receipts: a receipt still marked `running` is the exact
    // failure this tool exists for (a build step that dies mid-run and leaves a
    // stale receipt), and it is why every later launch retries the owed work.
    println!("\n[update receipts]");
    report_stale_receipts(home);

    // desktop app running (a blocker for the update phase) — fail closed
    match env::process_state("Hermes.exe") {
        env::ProcState::Running => {
            println!("\n[desktop app] RUNNING — update phase needs it closed")
        }
        env::ProcState::Absent => println!("\n[desktop app] not running"),
        env::ProcState::Unknown(why) => println!(
            "\n[desktop app] ! could not verify ({}) — treat as RUNNING",
            why
        ),
    }

    // provider drift status (read-only)
    println!("\n[provider surface drift]");
    drift_summary(home)?;

    // ---- context health -----------------------------------------------------
    // The transcript is a first-class health signal: a healthy DB can still be a
    // dying session. On 2026-10-19 the fleet carried 1,785 duplicate live rows
    // (2 active rows per message_uid) and 5.2x archive bloat - the actual cause
    // of the five-week "context too long" incident, invisible to a surface-only
    // tripwire.
    println!();
    let _ = crate::context_health::report(home)?;

    // A custom compression engine bypasses the built-in summariser. If the
    // summariser's route is also dead, the fleet cannot compress at all.
    println!("\n[compression engine]");
    let mut custom = false;
    for p in env::profiles(home) {
        if let Some(e) = crate::context_health::custom_compression_engine(home, &p) {
            println!("  ! {}: custom engine '{}' (built-in summariser bypassed)", p, e);
            custom = true;
        }
    }
    if !custom {
        println!("  (built-in everywhere)");
    }

    // gateway state (root)
    let gw = home.join("gateway_state.json");
    if gw.exists() {
        if let Ok(js) = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&gw)?) {
            let pid = js.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
            let st = js
                .get("gateway_state")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            println!("[gateway] pid={} state={}", pid, st);
        }
    }

    println!("\nPREFLIGHT DONE — see flags above.");
    Ok(())
}

/// Scan `logs/update_receipts/*.json` for updates that never finished.
/// A receipt with `outcome == "running"` or `finished_at == null` means the last
/// update died mid-flight; hermes keeps retrying the work it owed on every
/// launch until a real `hermes update` completes it.
/// Returns one line per unfinished receipt.
pub fn stale_receipts(home: &Path) -> Vec<String> {
    let dir = home.join("logs").join("update_receipts");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .map(|x| x.eq_ignore_ascii_case("json"))
                    .unwrap_or(false)
        })
        .collect();
    files.sort();
    let mut out: Vec<String> = Vec::new();
    for f in &files {
        let Ok(txt) = std::fs::read_to_string(f) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
            continue;
        };
        let outcome = v.get("outcome").and_then(|x| x.as_str()).unwrap_or("");
        let unfinished = v.get("finished_at").map(|x| x.is_null()).unwrap_or(false);
        if outcome != "running" && !unfinished {
            continue;
        }
        let started = v.get("started_at").and_then(|x| x.as_str()).unwrap_or("?");
        let target = v
            .get("plan")
            .and_then(|p| p.get("expected_version"))
            .and_then(|x| x.as_str())
            .unwrap_or("?");
        let pid = v.get("pid").and_then(|x| x.as_u64()).unwrap_or(0);
        // Liveness decides the meaning: the same "running" receipt is either an
        // update in flight or a corpse from a kill. Reporting a dead pid as
        // "mid-flight" (as this did for a receipt from 2026-10-09) teaches the
        // operator to ignore the check.
        let liveness = match crate::env::pid_running(pid as u32) {
            env::ProcState::Running => "pid LIVE - update in flight",
            env::ProcState::Absent => "pid dead - stale receipt, nothing in flight",
            env::ProcState::Unknown(_) => "pid unknown - cannot prove it dead",
        };
        out.push(format!(
            "{} : outcome={} finished_at=null (pid {}, {}, started {}, was pulling v{})",
            f.file_name().unwrap_or_default().to_string_lossy(),
            outcome,
            pid,
            liveness,
            started,
            target
        ));
    }
    out
}

fn report_stale_receipts(home: &Path) {
    let lines = stale_receipts(home);
    if lines.is_empty() {
        println!("  no receipt left in a running state");
    } else {
        for l in &lines {
            println!("  ! {}", l);
        }
        let live = lines.iter().filter(|l| l.contains("pid LIVE")).count();
        if live == 0 {
            println!(
                "  {} stale receipt(s), no live pid - history, not an update in flight",
                lines.len()
            );
        } else {
            println!(
                "  {} receipt(s) never finished, {} with a live pid - an update is genuinely in flight",
                lines.len(),
                live
            );
        }
    }
}

fn drift_summary(home: &Path) -> Result<()> {
    let surface = home.join("backups").join("provider-surface");
    let golden = surface.join("golden.json");
    let lk = surface.join("last-known");
    let baseline: Option<PathBuf> = if golden.exists() {
        Some(golden)
    } else if lk.is_dir() {
        Some(lk)
    } else {
        None
    };
    let Some(base) = baseline else {
        println!("  no golden/last-known baseline (run export first)");
        return Ok(());
    };
    let basef = base.clone();
    let lines = crate::drift::compare_against(home, &basef);
    if lines.is_empty() {
        println!("  all present profiles match baseline");
    } else {
        for l in &lines {
            println!("  ! {}", l.trim());
        }
        println!(
            "  {} profile(s) drifted",
            lines.iter().filter(|l| l.contains(" drifted vs ")).count()
        );
    }
    Ok(())
}
