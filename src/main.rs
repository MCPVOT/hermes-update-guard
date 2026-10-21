// clang-as-linker on windows-gnu/gnullvm emits a harmless "-no-pie" argument-unused message.
#![allow(linker_messages)]

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod backup;
mod context_health;
mod drift;
mod env;
mod guard;
mod logsignals;
mod mcpcheck;
mod preflight;
mod quarantine;
mod retire;
mod selfcheck;
mod state;
mod tui;
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

    /// A hermes-update-guard checkout to compare the installed binary against
    /// (also read from $HERMES_GUARD_REPO). Catches a STALE installed binary.
    #[arg(long, global = true)]
    repo: Option<PathBuf>,

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
    /// Move the APPROVED baseline (golden.json) to the current live surface.
    /// Deliberate operator action: this is the only thing that re-blesses the
    /// reference every drift verdict is measured against.
    Bless,
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
        /// Skip the read-only preflight gate (DANGEROUS: the gate is what protects
        /// every state.db). Renamed from --force, which read as "update even if
        /// current" while it actually means "ignore the safety checks".
        #[arg(long)]
        skip_preflight: bool,
        /// Deprecated alias for --skip-preflight.
        #[arg(long, hide = true)]
        force: bool,
        /// Run the update in its own detached process and return immediately, so a
        /// closed terminal or a caller timeout cannot kill it mid-build.
        #[arg(long)]
        detached: bool,
        /// Transient channel override for THIS run only (hermes keeps the
        /// persisted channel untouched). Use it to pull the newest main without
        /// moving the install off its release channel.
        #[arg(long)]
        channel: Option<String>,
    },
    /// Live view of a running (or finished) guarded update: stages, per-profile
    /// backup progress, blockers and the log tail.
    Watch {
        /// Print a one-shot text snapshot instead of the full-screen view.
        #[arg(long)]
        no_tui: bool,
    },
    /// Post-update verification (HEAD advanced, DB integrity, drift check, gateway state).
    Verify,
    /// Context-health audit: duplicate live rows, archive bloat, stuck compression
    /// sessions, oversized state.db. Exit 1 on problems. Read-only.
    Health {
        /// Audit one profile in depth (readiness + transcript health) instead of the fleet.
        #[arg(long)]
        profile: Option<String>,
        /// Also print the fleet's top log-noise signatures.
        #[arg(long)]
        logs: bool,
    },
    /// MCP server readiness: enabled servers, token state, headless-auth failures.
    Mcp,
    /// Snapshot the fleet's log signatures so the next run can attribute NEW error
    /// signatures to the update that introduced them.
    SnapshotLogs,
    /// Move stale update markers (.hermes-update-*) into a dated quarantine dir.
    /// Refuses to touch a marker whose owner pid is still alive. Nothing is deleted.
    Quarantine {
        /// Actually move them (without this it only prints the plan).
        #[arg(long)]
        apply: bool,
    },
    /// Record that a profile is GONE ON PURPOSE, so self-check stops flagging it as
    /// missing. Refuses if the profile still exists on disk.
    Retire {
        /// Profile name (the one self-check reported as absent).
        profile: String,
        /// Remove the record and let the flag come back.
        #[arg(long)]
        undo: bool,
    },
    /// Is the guard I am running the one I think I am? PATH binary + version,
    /// stale clones under the Hermes home, repo comparison, fleet, health verdict.
    SelfCheck,
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
        Cmd::Bless => drift::bless(&home),
        Cmd::Diff { baseline } => {
            let b = baseline.as_deref().and_then(resolve_baseline);
            drift::diff(&home, b.as_deref())
        }
        Cmd::Backup { dry_run } => backup::run(&home, dry_run).map(|_| ()),
        Cmd::Update {
            close_desktop,
            skip_preflight,
            force,
            detached,
            channel,
        } => {
            let skip = skip_preflight || force;
            if force && !skip_preflight {
                eprintln!("note: --force is deprecated; it means --skip-preflight (ignore the safety gate)");
            }
            guard::run(&home, close_desktop, skip, detached, channel.as_deref())
        }
        Cmd::Watch { no_tui } => {
            if no_tui {
                tui::snapshot(&home)
            } else {
                tui::run(&home)
            }
        }
        Cmd::Verify => verify::run(&home),
        Cmd::Mcp => mcpcheck::run(&home),
        Cmd::SnapshotLogs => {
            let sp = logsignals::snapshot(&home)?;
            println!("log-signal snapshot written: {}", sp.display());
            Ok(())
        }
        Cmd::Health { profile, logs } => match profile {
            Some(p) => {
                let bad = crate::context_health::report_readiness(&home, &p)?;
                if logs {
                    println!();
                    let _ = crate::logsignals::report(&home, 12);
                }
                if bad {
                    std::process::exit(1);
                }
                Ok(())
            }
            None => context_health::run_with(&home, logs),
        },
        Cmd::Quarantine { apply } => quarantine::run(&home, apply),
        Cmd::Retire { profile, undo } => retire::run(&home, &profile, undo),
        Cmd::SelfCheck => selfcheck::run(&home, cli.repo.as_deref()),
        Cmd::Vibe => {
            vibe::print_full();
            Ok(())
        }
    }
}
