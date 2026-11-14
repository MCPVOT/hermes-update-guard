// clang-as-linker on windows-gnu/gnullvm emits a harmless "-no-pie" argument-unused message.
#![allow(linker_messages)]

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod backup;
mod config;
mod context_health;
mod drift;
mod env;
mod guard;
mod lineage;
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

    /// Where `pre-update-<stamp>/` restore points go. Default:
    /// $HERMES_GUARD_BULK_ROOT, else <home>/backups. Must be a mount point
    /// unless it is the control root or --allow-local-backup is passed.
    #[arg(long, global = true, value_name = "PATH")]
    backup_root: Option<PathBuf>,

    /// Leave `.env` and `auth.json` out of the backup. Recorded in the manifest,
    /// so `verify` calls the restore point PARTIAL instead of complete.
    #[arg(long, global = true)]
    no_secrets: bool,

    /// Comma-separated content directories to copy: memories,skills,scripts,cron
    /// (default: all four).
    #[arg(long, global = true, value_name = "LIST")]
    content: Option<String>,

    /// Keep only the newest N pre-update runs on the bulk root (0 = keep all).
    /// Pruning happens only after a verified backup, and never removes the run
    /// just written.
    #[arg(long, global = true, default_value_t = 0, value_name = "N")]
    keep: usize,

    /// Accept a backup target that is not a mount point (the internal disk).
    /// Without this, an unplugged disk is refused instead of silently becoming
    /// a directory on the root filesystem.
    #[arg(long, global = true)]
    allow_local_backup: bool,

    /// Proceed even when the selected release would REWIND or DIVERGE from HEAD.
    /// The gate prints what it is switching from and to before continuing.
    #[arg(long, global = true)]
    allow_lineage_change: bool,

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
        /// Do not open the pop-up watch window. The window is only a viewer, so
        /// this changes nothing about the update itself.
        #[arg(long)]
        no_dashboard: bool,
    },
    /// Open the pop-up watch window for the latest run. The same window an
    /// `update` opens by itself, without starting an update.
    Dashboard,
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
        /// Also quarantine dead-pid update receipts (the "3 unfinished" line).
        /// Refuses any receipt whose pid cannot be proven dead.
        #[arg(long)]
        receipts: bool,
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
///
/// Takes the RESOLVED home, not `env::hermes_home()`: `--home` is a global flag,
/// and resolving "golden" against the default home made `--home /tmp/x diff
/// --baseline golden` read the REAL ~/.hermes baseline while reporting on the
/// temp home's profiles — a mismatch that reads as a clean pass.
fn resolve_baseline(home: &std::path::Path, s: &str) -> Option<std::path::PathBuf> {
    match s {
        "golden" => Some(
            home.join("backups")
                .join("provider-surface")
                .join("golden.json"),
        ),
        "last-known" => Some(
            home.join("backups")
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
    // Resolve the two roots and the flags ONCE. Modules must not re-derive the
    // bulk root: a silent fallback there could write a restore point onto the
    // internal disk while the operator believed it was on the removable one.
    let cfg = config::Config::new(
        &home,
        config::ConfigInput {
            backup_root: cli.backup_root.clone(),
            no_secrets: cli.no_secrets,
            content: cli.content.clone(),
            keep: cli.keep,
            allow_local_backup: cli.allow_local_backup,
            allow_lineage_change: cli.allow_lineage_change,
        },
    )?;
    match cli.cmd {
        Cmd::Preflight => preflight::run(&home, &cfg),
        Cmd::Export => drift::export(&home),
        Cmd::Bless => drift::bless(&home),
        Cmd::Diff { baseline } => {
            let b = baseline.as_deref().and_then(|s| resolve_baseline(&home, s));
            drift::diff(&home, b.as_deref())
        }
        Cmd::Backup { dry_run } => backup::run(&home, dry_run, &cfg).map(|_| ()),
        Cmd::Update {
            close_desktop,
            skip_preflight,
            force,
            detached,
            channel,
            no_dashboard,
        } => {
            let skip = skip_preflight || force;
            if force && !skip_preflight {
                eprintln!("note: --force is deprecated; it means --skip-preflight (ignore the safety gate)");
            }
            guard::run(
                &home,
                close_desktop,
                skip,
                detached,
                channel.as_deref(),
                !no_dashboard,
                &cfg,
            )
        }
        Cmd::Dashboard => guard::open_dashboard(&home),
        Cmd::Watch { no_tui } => {
            if no_tui {
                tui::snapshot(&home)
            } else {
                tui::run(&home)
            }
        }
        Cmd::Verify => verify::run(&home, &cfg),
        Cmd::Mcp => mcpcheck::run(&home),
        Cmd::SnapshotLogs => {
            let sp = logsignals::snapshot(&home)?;
            println!("log-signal snapshot written: {}", sp.display());
            Ok(())
        }
        Cmd::Health { profile, logs } => match profile {
            Some(p) => {
                let mut bad = crate::context_health::report_readiness(&home, &p)?;
                if logs {
                    println!();
                    // M1: an unreadable log report is a failed section, not an empty one.
                    if let Err(e) = crate::logsignals::report(&home, 12) {
                        println!("  [FAIL] log report could not run: {e}");
                        bad = true;
                    }
                }
                if bad {
                    std::process::exit(3);
                }
                Ok(())
            }
            None => context_health::run_with(&home, logs),
        },
        Cmd::Quarantine { apply, receipts } => quarantine::run(&home, apply, receipts),
        Cmd::Retire { profile, undo } => retire::run(&home, &profile, undo, &cfg.bulk_root),
        Cmd::SelfCheck => selfcheck::run(&home, cli.repo.as_deref(), &cfg.bulk_root),
        Cmd::Vibe => {
            vibe::print_full();
            Ok(())
        }
    }
}
