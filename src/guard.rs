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

        let holders = env::venv_holders(home);
        if !holders.is_empty() {
            bail!(
                "venv is held by: {} — update would refuse or strand the install. Stop them first.",
                holders.join(", ")
            );
        }

        let desktop = env::process_running("Hermes.exe");
        if desktop {
            if close_desktop {
                println!("  Desktop app running - closing it (forced)...");
                let _ = env::run_capture(
                    Path::new("taskkill"),
                    &["/IM", "Hermes.exe", "/F", "/T"],
                    None,
                );
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
        println!("  preflight gate passed");
    } else {
        println!("  preflight gate SKIPPED (--force)");
    }

    println!("\n-- DRIFT EXPORT (pre-update) --");
    drift::export(home)?;

    println!("\n-- WAL-SAFE BACKUP --");
    let backup_dir = backup::run(home, false)?;

    println!("\n-- HERMES UPDATE --");
    let hermes = env::venv_hermes(home);
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
