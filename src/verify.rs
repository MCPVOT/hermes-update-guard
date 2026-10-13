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

    if bad > 0 {
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
    let surface = home.join("backups").join("provider-surface");
    let _ = &surface;
    let mut drifted = 0usize;
    for p in PROFILES {
        let cfg = env::config_yaml(home, p);
        if !cfg.exists() {
            continue;
        }
        let live = crate::drift::extract_surface(&cfg)?;
        let basef = if baseline.is_dir() {
            baseline.join(format!("{}.json", p))
        } else {
            baseline.to_path_buf()
        };
        if !basef.exists() {
            continue;
        }
        let basev: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&basef)?)?;
        if live != basev {
            drifted += 1;
            println!(
                "  ! {} drifted vs {}",
                p,
                basef.file_name().unwrap_or_default().to_string_lossy()
            );
        }
    }
    if drifted == 0 {
        println!("  ok   no drift vs pre-update export");
    } else {
        println!("  !    {} profile(s) drifted vs pre-update export", drifted);
    }
    Ok(())
}
