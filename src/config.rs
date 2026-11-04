//! Run configuration shared by every subcommand.
//!
//! Resolved ONCE in `main` so no module re-derives the backup target or the
//! content selection from flags/env on its own. That matters most for the
//! two-root split: a module that quietly fell back to `<home>/backups` would
//! write a pre-update run onto the internal disk while the operator believed it
//! was on the removable one.

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

use crate::env;

/// Content directories a backup can include, in the order a default run copies them.
pub const CONTENT_KINDS: [&str; 4] = ["memories", "skills", "scripts", "cron"];

/// Everything the backup/verify/preflight paths need from the command line.
#[derive(Debug, Clone)]
pub struct Config {
    /// Always-local guard artefacts (`guard-run.json`, log, provider-surface, ...).
    pub control_root: PathBuf,
    /// Where `pre-update-<stamp>/` directories go. Configurable, may be a mounted disk.
    pub bulk_root: PathBuf,
    /// Skip `.env` and `auth.json`; recorded in the manifest.
    pub no_secrets: bool,
    /// Subset of [`CONTENT_KINDS`] to copy.
    pub content: Vec<String>,
    /// Keep only the newest N pre-update directories on the bulk root (0 = keep all).
    pub keep: usize,
    /// Accept a bulk root that is not a mount point (deliberate internal-disk use).
    pub allow_local_backup: bool,
    /// Accept a rewind/divergent release tag instead of blocking the update.
    pub allow_lineage_change: bool,
}

impl Config {
    /// Resolve the two roots and the flags without touching the filesystem.
    ///
    /// The target is deliberately NOT proved here: `verify` and `self-check` must
    /// keep working when the removable disk is detached, so only the commands
    /// that actually write a backup call [`env::prove_backup_target`].
    pub fn new(home: &Path, cli: ConfigInput) -> Result<Self> {
        Ok(Self {
            control_root: env::control_root(home),
            bulk_root: env::bulk_root(home, cli.backup_root.as_deref())?,
            no_secrets: cli.no_secrets,
            content: parse_content(cli.content.as_deref())?,
            keep: cli.keep,
            allow_local_backup: cli.allow_local_backup,
            allow_lineage_change: cli.allow_lineage_change,
        })
    }

    pub fn wants_content(&self, kind: &str) -> bool {
        self.content.iter().any(|c| c == kind)
    }

    /// The bulk root is the default control root, so no mount proof is required.
    pub fn bulk_is_control(&self) -> bool {
        self.bulk_root == self.control_root
    }
}

/// Raw CLI values, kept separate so [`Config::new`] is testable without clap.
#[derive(Debug, Clone, Default)]
pub struct ConfigInput {
    pub backup_root: Option<PathBuf>,
    pub no_secrets: bool,
    pub content: Option<String>,
    pub keep: usize,
    pub allow_local_backup: bool,
    pub allow_lineage_change: bool,
}

/// Parse and validate a `--content` list. `None` means all four.
pub fn parse_content(raw: Option<&str>) -> Result<Vec<String>> {
    let Some(raw) = raw else {
        return Ok(CONTENT_KINDS.iter().map(|s| s.to_string()).collect());
    };
    let mut out: Vec<String> = Vec::new();
    for part in raw.split(',') {
        let name = part.trim();
        if name.is_empty() {
            continue;
        }
        if !CONTENT_KINDS.contains(&name) {
            bail!(
                "unknown --content value '{}' (choose from {})",
                name,
                CONTENT_KINDS.join(",")
            );
        }
        if !out.iter().any(|x| x == name) {
            out.push(name.to_string());
        }
    }
    if out.is_empty() {
        bail!(
            "--content was given but listed nothing; choose from {}",
            CONTENT_KINDS.join(",")
        );
    }
    // Canonical order, so the copy order (and the plan output) is diffable.
    Ok(CONTENT_KINDS
        .iter()
        .filter(|k| out.iter().any(|x| x == *k))
        .map(|s| s.to_string())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_defaults_to_all_four_in_order() {
        assert_eq!(
            parse_content(None).unwrap(),
            vec![
                "memories".to_string(),
                "skills".to_string(),
                "scripts".to_string(),
                "cron".to_string()
            ]
        );
    }

    #[test]
    fn content_subset_is_trimmed_deduped_and_ordered() {
        assert_eq!(
            parse_content(Some(" skills , memories,skills ,,")).unwrap(),
            vec!["memories".to_string(), "skills".to_string()]
        );
    }

    #[test]
    fn unknown_content_is_refused_not_ignored() {
        // Silently dropping an unknown name would produce a backup that looks
        // complete but never contained the directory the operator asked for.
        assert!(parse_content(Some("memories,secrets")).is_err());
        assert!(parse_content(Some(" , ")).is_err());
    }
}
