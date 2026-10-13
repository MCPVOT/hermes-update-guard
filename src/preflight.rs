use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::env::{self, PROFILES};

/// Read-only preflight: surface every condition that decides an update.
pub fn run(home: &Path) -> Result<()> {
    println!("== HERMES UPDATE PREFLIGHT ==");
    println!("home: {}", home.display());

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
    println!("  {}", cout.trim().lines().next().unwrap_or("(empty)"));

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
        let (_, behind) = env::run_capture(
            Path::new("git"),
            &[
                "-C",
                repo.to_str().unwrap(),
                "rev-list",
                "--count",
                "HEAD..@{u}",
            ],
            None,
        )
        .unwrap_or((-1, "?".into()));
        let (_, stashes) = env::run_capture(
            Path::new("git"),
            &["-C", repo.to_str().unwrap(), "stash", "list"],
            None,
        )
        .unwrap_or((-1, String::new()));
        println!("  HEAD     : {}", head.trim());
        println!("  behind   : {} commits", behind.trim());
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
    let holders = env::venv_holders(home);
    if holders.is_empty() {
        println!("  (none)");
    } else {
        for h in &holders {
            println!("  {}", h);
        }
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

    // desktop app running (a blocker for the update phase)
    let desktop = env::process_running("Hermes.exe");
    println!(
        "\n[desktop app] {}",
        if desktop {
            "RUNNING — update phase needs it closed"
        } else {
            "not running"
        }
    );

    // provider drift status (read-only)
    println!("\n[provider surface drift]");
    drift_summary(home)?;

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
    let mut drifted = 0usize;
    for p in PROFILES {
        let cfg = env::config_yaml(home, p);
        if !cfg.exists() {
            continue;
        }
        let live = crate::drift::extract_surface(&cfg)?;
        let basef = if base.is_dir() {
            base.join(format!("{}.json", p))
        } else {
            base.to_path_buf()
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
        println!("  all present profiles match baseline");
    } else {
        println!("  {} profile(s) drifted", drifted);
    }
    Ok(())
}
