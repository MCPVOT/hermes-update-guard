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

    // Read every marker and ask what its owner is doing. `Unknown` (the probe
    // could not answer) is treated like `Running`, because moving a marker
    // because we could not tell whether it was live is exactly the mistake that
    // breaks a real update.
    let mut entries: Vec<(String, std::path::PathBuf, Option<u32>, env::ProcState)> = Vec::new();
    for (name, p) in markers(home) {
        let pid = marker_pid(&p);
        let state = match pid {
            Some(v) => env::pid_running(v),
            None => env::ProcState::Absent,
        };
        entries.push((name, p, pid, state));
    }

    if entries.is_empty() {
        println!("  no stale update markers to move");
        return Ok(());
    }

    // Markers are a SET, not independent files: an empty `.lock` belongs to
    // whatever run wrote the pid into the sibling `.hermes-update-in-progress`.
    // Moving the empty lock while that run is alive would break a real update,
    // so any live (or unverifiable) owner holds the entire set.
    let held: Vec<&(String, std::path::PathBuf, Option<u32>, env::ProcState)> = entries
        .iter()
        .filter(|(_, _, _, s)| !matches!(s, env::ProcState::Absent))
        .collect();

    let fmt_pid = |p: Option<u32>| p.map(|v| v.to_string()).unwrap_or_else(|| "?".into());
    let reason = |s: &env::ProcState| match s {
        env::ProcState::Running => "LIVE".to_string(),
        env::ProcState::Unknown(why) => format!("UNVERIFIABLE ({why})"),
        env::ProcState::Absent => "absent".into(),
    };

    if !held.is_empty() {
        for (name, _, pid, state) in &held {
            println!(
                "  ! {} owner {} - an update really is in flight; not touching it",
                name,
                reason(state)
            );
            let _ = pid;
        }
        println!(
            "\nREFUSING to quarantine: {} marker(s) in this home are held. Nothing moved.",
            held.len()
        );
        return Ok(());
    }

    for (name, _, pid, _) in &entries {
        println!(
            "  - {} (owner pid {}) - {}",
            name,
            fmt_pid(*pid),
            if apply { "will move" } else { "would move" }
        );
    }

    if !apply {
        println!("\nDRY RUN - nothing moved. Re-run with --apply to quarantine them.");
        return Ok(());
    }

    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let dest = home
        .join("backups")
        .join("update-state-quarantine")
        .join(&stamp);
    std::fs::create_dir_all(&dest)?;
    let mut moved = 0usize;
    for (name, path, _, _) in &entries {
        let target = dest.join(name);
        std::fs::rename(path, &target)?;
        println!("  moved {} -> {}", name, target.display());
        moved += 1;
    }
    println!("\nQuarantined {} marker(s) into {}", moved, dest.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real markers start with a bare pid line followed by timestamps.
    #[test]
    fn marker_pid_reads_the_pid_line() {
        let d = std::env::temp_dir().join(format!("hug-q-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join(".hermes-update-in-progress");
        std::fs::write(&f, "27224\n1791896026\nct:1791896014.929\n").unwrap();
        assert_eq!(marker_pid(&f), Some(27224));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An empty lock has no owner, which is why the aggregate hold exists:
    /// the empty file itself can never be judged live on its own.
    #[test]
    fn marker_pid_is_none_for_empty_or_garbage() {
        let d = std::env::temp_dir().join(format!("hug-q2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let empty = d.join(".hermes-update-in-progress.lock");
        std::fs::write(&empty, "").unwrap();
        assert_eq!(marker_pid(&empty), None);
        let junk = d.join(".hermes-update-other");
        std::fs::write(&junk, "not-a-pid\n").unwrap();
        assert_eq!(marker_pid(&junk), None);
        assert_eq!(marker_pid(&d.join("nope")), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Only `.hermes-update*` entries in the home are markers; `.deleted` and
    /// unrelated files must never be touched.
    #[test]
    fn markers_are_prefix_scoped() {
        let d = std::env::temp_dir().join(format!("hug-q3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(".hermes-update-in-progress.lock"), "").unwrap();
        std::fs::write(d.join(".deleted"), "elevate").unwrap();
        std::fs::write(d.join("config.yaml"), "x: 1\n").unwrap();
        let names: Vec<String> = markers(&d).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec![".hermes-update-in-progress.lock".to_string()]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
