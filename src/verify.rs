use anyhow::Result;
use std::path::Path;

use crate::env::{self, PROFILES};

/// Post-update verification: HEAD advanced, version sane, every live db integrity ok,
/// provider surface matches the pre-update export, gateway state present.
pub fn run(home: &Path) -> Result<()> {
    println!("== POST-UPDATE VERIFY ==");

    let repo = home.join("hermes-agent");
    let (_, head) = env::run_capture(
        Path::new("git"),
        &["-C", repo.to_str().unwrap(), "rev-parse", "--short", "HEAD"],
        None,
    )
    .unwrap_or((-1, "?".into()));
    println!("  HEAD now : {}", head.trim());

    let (_, version) =
        env::run_capture(&env::venv_hermes(home), &["--version"], None).unwrap_or((-1, "?".into()));
    println!(
        "  version  : {}",
        version.lines().next().unwrap_or("?").trim()
    );

    // every live db integrity check (read-only)
    let mut bad = 0usize;
    let mut patch_bad = 0usize;
    for p in PROFILES {
        let db = env::state_db(home, p);
        if !db.exists() {
            continue;
        }
        match integrity(&db) {
            Ok((ok, msgs)) => {
                if ok != "ok" {
                    bad += 1;
                    println!("  FAIL {} integrity={}", p, ok);
                } else {
                    println!("  ok   {} integrity=ok messages={}", p, msgs);
                }
            }
            Err(e) => {
                bad += 1;
                println!("  FAIL {} {}", p, e);
            }
        }
    }
    if bad > 0 {
        println!("  [FAIL] {} database(s) have integrity problems", bad);
    }

    // local patch restoration: the updater stashes tracked modifications, pulls,
    // then re-applies them. A clean tree WITH stash entries left behind means it
    // parked your patch instead of restoring it - a silent loss of a pinned core
    // file (e.g. models_catalog_static.py).
    println!("  local patch (stash/restore):");
    let (_, dirty_now) = env::run_capture(
        Path::new("git"),
        &["-C", repo.to_str().unwrap(), "status", "--short"],
        None,
    )
    .unwrap_or((-1, String::new()));
    let (_, stashes) = env::run_capture(
        Path::new("git"),
        &["-C", repo.to_str().unwrap(), "stash", "list"],
        None,
    )
    .unwrap_or((-1, String::new()));
    let dirty_lines: Vec<&str> = dirty_now.lines().filter(|l| !l.trim().is_empty()).collect();
    let stash_lines: Vec<&str> = stashes.lines().filter(|l| !l.trim().is_empty()).collect();
    if !dirty_lines.is_empty() {
        println!(
            "  ok   {} local modification(s) re-applied on top of the update:",
            dirty_lines.len()
        );
        for d in &dirty_lines {
            println!("         {}", d.trim());
        }
    } else if stash_lines.is_empty() {
        println!("  ok   working tree clean, no stash entries (nothing was pinned)");
    } else {
        patch_bad += 1;
        println!(
            "  FAIL tree is clean but {} stash entr{} remain - the update parked your local patch",
            stash_lines.len(),
            if stash_lines.len() == 1 { "y" } else { "ies" }
        );
        for s in &stash_lines {
            println!("         {}", s.trim());
        }
    }

    // drift vs the newest export (post-update diff)
    println!("  provider surface vs last export:");
    let surface = home.join("backups").join("provider-surface");
    let pointer = surface.join("last-export.txt");
    if pointer.exists() {
        if let Ok(stamp) = std::fs::read_to_string(&pointer) {
            let baseline = surface.join(stamp.trim());
            if baseline.is_dir() {
                let _ = drift_diff_quiet(home, &baseline);
            }
        }
    }

    // gateway state (root multiplexed)
    let gw = home.join("gateway_state.json");
    if gw.exists() {
        if let Ok(js) = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&gw)?) {
            println!(
                "  gateway  : pid={} state={}",
                js.get("pid").and_then(|v| v.as_u64()).unwrap_or(0),
                js.get("gateway_state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
            );
        }
    }

    if bad > 0 || patch_bad > 0 {
        println!("\nVERIFY: FAIL");
    } else {
        println!("\nVERIFY: PASS");
    }
    Ok(())
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

fn drift_diff_quiet(home: &Path, baseline: &Path) -> Result<()> {
    let lines = crate::drift::compare_against(home, baseline);
    if lines.is_empty() {
        println!("  ok   no drift vs pre-update export");
    } else {
        for l in &lines {
            println!("  ! {}", l.trim());
        }
        println!(
            "  !    {} profile(s) drifted vs pre-update export",
            lines.len()
        );
    }
    Ok(())
}
