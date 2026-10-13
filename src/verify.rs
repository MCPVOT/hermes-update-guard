use anyhow::Result;
use std::path::Path;

use crate::env::{self, PROFILES};

/// Post-update verification: HEAD advanced, version sane, every live db integrity ok,
/// provider surface matches the pre-update export, gateway state present.
pub fn run(home: &Path) -> Result<()> {
    println!("== POST-UPDATE VERIFY ==");

    let mut bad = 0usize;

    // ---- HEAD (git) — a check that cannot run must FAIL, never print "?" and still pass ----
    let repo = home.join("hermes-agent");
    let repo_s = repo.to_str().unwrap_or(".");
    match env::run_capture(
        Path::new("git"),
        &["-C", repo_s, "rev-parse", "--short", "HEAD"],
        None,
    ) {
        Ok((code, out)) if code == 0 && first_line_is_real(&out) => {
            println!("  HEAD now : {}", first_line(&out));
        }
        Ok((code, out)) => {
            bad += 1;
            println!(
                "  FAIL HEAD unreadable at {} (exit {}): {}",
                repo_s,
                code,
                first_line(&out)
            );
        }
        Err(e) => {
            bad += 1;
            println!("  FAIL HEAD check could not run: {}", e);
        }
    }

    // ---- version ----
    let hermes_bin = env::venv_hermes(home);
    match env::run_capture(&hermes_bin, &["--version"], None) {
        Ok((code, out)) if code == 0 && first_line_is_real(&out) => {
            println!("  version  : {}", first_line(&out));
        }
        Ok((code, out)) => {
            bad += 1;
            println!(
                "  FAIL version unreadable at {} (exit {}): {}",
                hermes_bin.display(),
                code,
                first_line(&out)
            );
        }
        Err(e) => {
            bad += 1;
            println!(
                "  FAIL version check could not run ({}): {}",
                hermes_bin.display(),
                e
            );
        }
    }

    // every live db integrity check (read-only)
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
        println!("  [FAIL] {} check(s) failed so far", bad);
    }

    // ---- drift vs the newest export — this DECIDES the verdict, it does not decorate it ----
    println!("  provider surface vs last export:");
    let surface = home.join("backups").join("provider-surface");
    let pointer = surface.join("last-export.txt");
    if !pointer.exists() {
        println!("  !    no export baseline yet — drift NOT checked (run: export)");
    } else {
        match std::fs::read_to_string(&pointer) {
            Ok(stamp) => {
                let baseline = surface.join(stamp.trim());
                if !baseline.is_dir() {
                    bad += 1;
                    println!(
                        "  FAIL export baseline {} referenced by last-export.txt is missing",
                        baseline.display()
                    );
                } else {
                    let lines = crate::drift::compare_against(home, &baseline);
                    if lines.is_empty() {
                        println!("  ok   no drift vs pre-update export");
                    } else {
                        bad += 1;
                        for l in &lines {
                            println!("  ! {}", l.trim());
                        }
                        println!(
                            "  FAIL {} profile(s) drifted vs pre-update export",
                            lines.len()
                        );
                    }
                }
            }
            Err(e) => {
                bad += 1;
                println!("  FAIL could not read last-export.txt: {}", e);
            }
        }
    }

    // gateway state (root multiplexed)
    let gw = home.join("gateway_state.json");
    if gw.exists() {
        match serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&gw)?) {
            Ok(js) => {
                println!(
                    "  gateway  : pid={} state={}",
                    js.get("pid").and_then(|v| v.as_u64()).unwrap_or(0),
                    js.get("gateway_state")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                );
            }
            Err(e) => {
                bad += 1;
                println!("  FAIL gateway_state.json is not valid JSON: {}", e);
            }
        }
    }

    if bad > 0 {
        println!("\nVERIFY: FAIL ({} check(s))", bad);
        anyhow::bail!("{} verification check(s) failed", bad);
    }
    println!("\nVERIFY: PASS");
    Ok(())
}

/// The first non-empty line of captured output.
fn first_line(s: &str) -> String {
    s.trim().lines().next().unwrap_or("").trim().to_string()
}

/// Real output only when non-empty and not the `?` placeholder.
fn first_line_is_real(s: &str) -> bool {
    let f = first_line(s);
    !f.is_empty() && f != "?"
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
