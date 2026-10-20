//! `quarantine` - move stale update markers out of the way.
//!
//! WHY
//!   An interrupted update leaves `.hermes-update-in-progress` /
//!   `.hermes-update-*.lock` behind, and Hermes then treats the install as
//!   "an update is in flight" - every command blocks on the reconcile, and the
//!   next update refuses. This happened three times in 2026-10 alone.
//!
//!   The markers are state, not data: nothing is lost by moving them into
//!   `backups/update-state-quarantine/<stamp>/`. The marker CONTENTS are kept
//!   (pid, timestamps) so an interrupted run can still be attributed.
//!
//! SAFETY
//!   - Read-only by default: it PRINTS what it would move. `--apply` moves.
//!   - It refuses to touch a marker whose owner pid is still ALIVE: that is a
//!     real update in flight, and moving its marker would break it.
//!   - Nothing is ever deleted; everything is moved.

use anyhow::Result;
use std::path::Path;

use crate::env;

const MARKER_PREFIX: &str = ".hermes-update";

/// Every update-state marker in the home, whatever its exact name.
/// A fixed list misses variants (`.*.claim`, `.retired`, ...) that different
/// Hermes versions leave behind, and every one of them can block a later update.
fn markers(home: &Path) -> Vec<(String, std::path::PathBuf)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(home) else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with(MARKER_PREFIX) {
            out.push((name, e.path()));
        }
    }
    out.sort();
    out
}

/// The pid recorded in a marker file, if any.
fn marker_pid(path: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .next()
        .and_then(|l| l.trim().parse::<u32>().ok())
}

pub fn run(home: &Path, apply: bool) -> Result<()> {
    println!("== UPDATE-STATE QUARANTINE ==");
    let mut movable: Vec<(String, std::path::PathBuf, Option<u32>)> = Vec::new();
    let mut blocked: Vec<(String, Option<u32>)> = Vec::new();

    for (name, p) in markers(home) {
        let pid = marker_pid(&p);
        let alive = pid.map(|v| matches!(env::pid_running(v), env::ProcState::Running)).unwrap_or(false);
        if alive {
            blocked.push((name, pid));
        } else {
            movable.push((name, p, pid));
        }
    }

    if blocked.is_empty() && movable.is_empty() {
        println!("  no stale update markers to move");
        return Ok(());
    }
    for (name, pid) in &blocked {
        println!(
            "  ! {} is HELD by a LIVE process (pid {}) - an update really is in flight; not touching it",
            name,
            pid.map(|v| v.to_string()).unwrap_or_else(|| "?".into())
        );
    }
    for (name, _, pid) in &movable {
        println!(
            "  - {} (owner pid {}) - {}",
            name,
            pid.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            if apply { "will move" } else { "would move" }
        );
    }

    if !apply {
        println!("\nDRY RUN - nothing moved. Re-run with --apply to quarantine them.");
        return Ok(());
    }
    if movable.is_empty() {
        println!("\nNothing to move.");
        return Ok(());
    }

    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let dest = home
        .join("backups")
        .join("update-state-quarantine")
        .join(&stamp);
    std::fs::create_dir_all(&dest)?;
    for (name, path, _) in &movable {
        let target = dest.join(name);
        std::fs::rename(path, &target)?;
        println!("  moved {} -> {}", name, target.display());
    }
    println!(
        "\nQuarantined {} marker(s) into {}",
        movable.len(),
        dest.display()
    );
    Ok(())
}
