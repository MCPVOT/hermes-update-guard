//! `retire` — record that a profile is GONE ON PURPOSE.
//!
//! WHY
//!   `self-check` compares the live fleet against every `pre-update-*` backup and
//!   reports profiles that were there but are not any more. That is the check that
//!   caught a whole profile disappearing. But an INTENTIONAL deletion is
//!   indistinguishable from an accidental one by looking at the disk, and a checker
//!   that keeps flagging a decision the operator already made is a checker people
//!   learn to ignore.
//!
//!   So the decision is recorded, with a date and the last backup that saw the
//!   profile, in `backups/profile-retired.json`. Same shape as `bless`: an explicit
//!   operator action moves the reference, never a side effect of a check.
//!
//! SAFETY
//!   - Refuses to retire a profile that still exists on disk: that is not a
//!     retirement, it is a live profile, and hiding it would be a regression.
//!   - `--undo` removes the record, so the flag comes back if the profile is still
//!     absent. Nothing is deleted either way.

use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;
use std::path::Path;

fn file(home: &Path) -> std::path::PathBuf {
    home.join("backups").join("profile-retired.json")
}

fn load(path: &Path) -> serde_json::Map<String, serde_json::Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Default::default();
    };
    let text = text.trim_start_matches('\u{feff}');
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => Default::default(),
    }
}

fn save(path: &Path, map: &serde_json::Map<String, serde_json::Value>) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(&serde_json::Value::Object(map.clone()))?,
    )
    .with_context(|| format!("write {}", path.display()))
}

/// Profiles recorded as intentionally retired.
pub fn retired(home: &Path) -> BTreeSet<String> {
    load(&file(home)).into_iter().map(|(k, _)| k).collect()
}

pub fn run(home: &Path, profile: &str, undo: bool) -> Result<()> {
    let path = file(home);
    let mut map = load(&path);

    if undo {
        if map.remove(profile).is_none() {
            println!("  {} was not retired - nothing to undo", profile);
            return Ok(());
        }
        save(&path, &map)?;
        println!(
            "  {} is no longer retired; self-check will flag it again while it is absent",
            profile
        );
        return Ok(());
    }

    // A live profile is not a retired one. Refuse rather than hide it: a profile
    // that exists but is silently filtered out of the fleet checks is exactly the
    // class of bug this tool exists to prevent.
    if crate::env::profiles(home).iter().any(|p| p == profile) {
        bail!(
            "{} still exists on disk - `retire` is for a profile that is GONE. Nothing recorded.",
            profile
        );
    }

    let last_seen = crate::env::last_seen_in_backups(home)
        .get(profile)
        .map(|s| s.trim_start_matches("pre-update-").to_string())
        .unwrap_or_else(|| "never".into());

    map.insert(
        profile.to_string(),
        serde_json::json!({
            "retired_at": chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
            "last_seen": last_seen,
        }),
    );
    save(&path, &map)?;

    println!(
        "  {} retired (last seen {}). self-check will no longer flag it.",
        profile, last_seen
    );
    println!("  undo: hermes-update-guard retire {} --undo", profile);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(tag: &str) -> std::path::PathBuf {
        let h = std::env::temp_dir().join(format!("hug-retire-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(h.join("profiles/live")).unwrap();
        std::fs::create_dir_all(h.join("backups/pre-update-20260101-000000/gone")).unwrap();
        std::fs::write(
            h.join("backups/pre-update-20260101-000000/gone/state.db"),
            "x",
        )
        .unwrap();
        h
    }

    #[test]
    fn retire_records_and_undo_restores_the_flag() {
        let h = home("undo");
        assert!(profiles_gone(&h).contains(&"gone (last seen 20260101-000000)".to_string()));

        run(&h, "gone", false).unwrap();
        assert!(retired(&h).contains("gone"));
        assert!(
            profiles_gone(&h).is_empty(),
            "a retired profile must not be flagged"
        );

        run(&h, "gone", true).unwrap();
        assert!(!retired(&h).contains("gone"));
        assert!(
            profiles_gone(&h).contains(&"gone (last seen 20260101-000000)".to_string()),
            "undo must bring the flag back"
        );
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn refuse_to_retire_a_live_profile() {
        let h = home("live");
        let err = run(&h, "live", false).unwrap_err().to_string();
        assert!(err.contains("still exists on disk"), "got: {err}");
        assert!(!retired(&h).contains("live"));
        let _ = std::fs::remove_dir_all(&h);
    }

    fn profiles_gone(h: &Path) -> Vec<String> {
        crate::env::profiles_gone_since_backup(h, &retired(h))
    }
}
