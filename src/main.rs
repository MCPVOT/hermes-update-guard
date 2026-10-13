// clang-as-linker on windows-gnu/gnullvm emits a harmless "-no-pie" argument-unused message.
#![allow(linker_messages)]

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod backup;
mod drift;
mod env;
mod guard;
mod preflight;
mod verify;
mod vibe;

#[derive(Parser)]
#[command(
    name = "hermes-update-guard",
    version,
    about = "Guardian orchestrator for safe Hermes Agent updates (preflight -> drift export -> WAL-safe backup -> hermes update -> post-verify -> restore)"
)]
struct Cli {
    /// Explicit hermes home root (default: env HERMES_HOME when it is a root, else %LOCALAPPDATA%/hermes or ~/.hermes).
    #[arg(long, global = true)]
    home: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Read-only state check before an update (version, git state, venv holders, locks, drift).
    Preflight,
    /// Export a provider-surface snapshot (run BEFORE hermes update).
    Export,
    /// Diff live provider surface against golden/last-known. Exit 1 on drift.
    Diff {
        /// Baseline choice: "golden" (default when present), "last-known", or an explicit path.
        #[arg(long)]
        baseline: Option<String>,
    },
    /// WAL-safe backup of every profile's state.db + content dirs. No update.
    Backup {
        /// Only print the backup plan, skip the run.
        #[arg(long)]
        dry_run: bool,
    },
    /// Full guarded update: preflight -> export -> backup -> hermes update -> verify.
    Update {
        /// Close the Desktop app as part of preflight (forced stop).
        #[arg(long)]
        close_desktop: bool,
        /// Skip the read-only preflight gate (dangerous).
        #[arg(long)]
        force: bool,
    },
    /// Post-update verification (HEAD advanced, DB integrity, drift check, gateway state).
    Verify,
    /// Print the MCPVOT identity vibe banner.
    Vibe,
}

/// Map a --baseline string to an explicit baseline path (None = auto: golden, else last-known).
fn resolve_baseline(s: &str) -> Option<std::path::PathBuf> {
    let h = env::hermes_home().ok()?;
    match s {
        "golden" => Some(
            h.join("backups")
                .join("provider-surface")
                .join("golden.json"),
        ),
        "last-known" => Some(
            h.join("backups")
                .join("provider-surface")
                .join("last-known"),
        ),
        other => Some(std::path::PathBuf::from(other)),
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let home = match &cli.home {
        Some(p) => env::hermes_home_from(p)?,
        None => env::hermes_home()?,
    };
    match cli.cmd {
        Cmd::Preflight => preflight::run(&home),
        Cmd::Export => drift::export(&home),
        Cmd::Diff { baseline } => {
            let b = baseline.as_deref().and_then(resolve_baseline);
            drift::diff(&home, b.as_deref())
        }
        Cmd::Backup { dry_run } => backup::run(&home, dry_run).map(|_| ()),
        Cmd::Update {
            close_desktop,
            force,
        } => guard::run(&home, close_desktop, force),
        Cmd::Verify => verify::run(&home),
        Cmd::Vibe => {
            vibe::print_full();
            Ok(())
        }
    }
}
