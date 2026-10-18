use anyhow::{bail, Result};
use std::path::Path;

use crate::backup;
use crate::drift;
use crate::env;

/// Full guarded update: preflight -> drift export -> WAL-safe backup -> hermes update --yes -> verify.
/// This is the Rust analogue of update-hermes.ps1 -Update (without -CloseDesktop, the caller
/// stops the Desktop app first when requested).
pub fn run(home: &Path, close_desktop: bool, force: bool) -> Result<()> {
    crate::vibe::print_compact(env!("CARGO_PKG_VERSION"));
    println!("== HERMES UPDATE GUARDIAN ==");
    if !force {
        println!("\n-- PREFLIGHT GATE --");

        // --list-venv-holders exits 3 when holders are present, so the probe's
        // exit code is part of the answer, not an error to be discarded. An
        // unparseable answer (hermes is mid-update) counts as held.
        let (holders, vexit, parsed) = env::venv_holders_probe(home);
        if !parsed || !holders.is_empty() || vexit == 3 {
            bail!(
                "venv is held by: {} (probe exit {}, json={}) — update would refuse or strand the install. Stop them first.",
                holders.join(", "),
                vexit,
                parsed
            );
        }

        let desktop = env::process_running("Hermes.exe");
        if desktop {
            if close_desktop {
                println!("  Desktop app running - closing it (forced)...");
                let killed = env::kill_process("Hermes.exe");
                if !killed {
                    bail!("failed to stop the Desktop app - close it and re-run");
                }
                std::thread::sleep(std::time::Duration::from_secs(6));
                if env::process_running("Hermes.exe") {
                    bail!("Desktop app still running after kill - close it and re-run");
                }
            } else {
                bail!("Desktop app is RUNNING - pass --close-desktop or close it manually");
            }
        } else {
            println!("  Desktop app not running");
        }

        for m in [
            ".hermes-update-in-progress",
            ".hermes-update-in-progress.lock",
        ] {
            if home.join(m).exists() {
                eprintln!(
                    "  [warn] stale marker {} present - previous update did not finish",
                    m
                );
            }
        }
        // an unfinished receipt means a real `hermes update` is still owed
        let stale = crate::preflight::stale_receipts(home);
        if !stale.is_empty() {
            for s in &stale {
                eprintln!("  [warn] unfinished update receipt: {}", s);
            }
            eprintln!(
                "  [warn] {} receipt(s) left running — finish the owed update before starting another",
                stale.len()
            );
        }
        println!("  preflight gate passed");
    } else {
        println!("  preflight gate SKIPPED (--force)");
    }

    println!("\n-- DRIFT EXPORT (pre-update) --");
    drift::export(home)?;

    println!("\n-- WAL-SAFE BACKUP --");
    let backup_dir = backup::run(home, false)?;

    println!("\n-- HERMES UPDATE --");
    let hermes = match env::resolve_hermes(home) {
        Some(p) => p,
        None => {
            bail!("no hermes CLI found (venv, PATH, ~/.local/bin, /usr/local/bin) — cannot update")
        }
    };
    println!("  using {}", hermes.display());
    let (code, out) = env::run_capture(&hermes, &["update", "--yes"], None)?;

    if !out.is_empty() {
        println!("{}", out);
    }
    if code != 0 {
        println!("\n*** UPDATE FAILED (exit {}) ***", code);
        println!("    backup retained at: {}", backup_dir.display());
        println!("    run `hermes-update-guard verify` only after you restore/harden");
        return Err(anyhow::anyhow!(
            "hermes update failed (exit {}) - backup at {} for restore",
            code,
            backup_dir.display()
        ));
    }
    println!("  hermes update exit 0");

    println!("\n-- POST-UPDATE VERIFY --");
    crate::verify::run(home)?;

    println!(
        "\nUPDATE FLOW COMPLETE - backup at {}",
        backup_dir.display()
    );
    Ok(())
}
