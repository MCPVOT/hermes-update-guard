//! Release-lineage gate.
//!
//! The install on the target machine is a canary checkout that is AHEAD of the
//! stable channel: HEAD is `v0.22.5+canary...-112-g088f657ecf`, and stable offers
//! `v0.22.7`, which is an ANCESTOR of HEAD. Updated blindly, the channel
//! resolver would walk the tree BACKWARDS by 1,075 commits and call it an update.
//! The gate classifies the move before it happens and blocks anything that is not
//! a fast-forward.
//!
//! No new dependency: the whole verdict is two `git merge-base --is-ancestor`
//! tests in opposite directions plus two `rev-list --count`s.

use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::env;

/// What the move from HEAD to the selected release tag would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lineage {
    /// The tag is not in the local object store (no fetch has run yet).
    TagMissing,
    /// HEAD already points at the tag's commit.
    UpToDate { sha: String },
    /// The tag is a descendant of HEAD: a clean, reversible move forward.
    FastForward { add: u64, drop: u64 },
    /// HEAD is a descendant of the tag: the update would walk the tree backwards.
    Rewind { add: u64, drop: u64 },
    /// Neither is an ancestor of the other: histories have forked.
    Divergent { add: u64, drop: u64 },
}

/// Pull the release tag out of `hermes update --check` output.
///
/// The documented line is `Selected release available: stable v0.22.7 (088f657)`;
/// older builds print `Update available: ...`. Only a version-looking token is
/// accepted, so the `(sha)` suffix can never be mistaken for the tag.
pub fn parse_target_tag(out: &str) -> Option<String> {
    for line in out.lines() {
        if !(line.contains("Selected release available") || line.contains("Update available")) {
            continue;
        }
        for token in line.split_whitespace() {
            let t = token.trim_matches(|c: char| {
                !(c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
            });
            let Some(rest) = t.strip_prefix(['v', 'V']) else {
                continue;
            };
            if rest.starts_with(|c: char| c.is_ascii_digit()) && rest.contains('.') {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// Classify the move from the checkout's HEAD to `tag`.
pub fn classify(repo: &Path, tag: &str) -> Result<Lineage> {
    let repo_s = repo.to_string_lossy().into_owned();
    let git = Path::new("git");

    let (tag_code, tag_out) = env::run_capture(
        git,
        &[
            "-C",
            &repo_s,
            "rev-parse",
            "--verify",
            &format!("{tag}^{{commit}}"),
        ],
        None,
    )?;
    if tag_code != 0 {
        return Ok(Lineage::TagMissing);
    }
    let tag_sha = tag_out.lines().next().unwrap_or("").trim().to_string();

    let (head_code, head_out) = env::run_capture(git, &["-C", &repo_s, "rev-parse", "HEAD"], None)?;
    if head_code != 0 {
        bail!("cannot resolve HEAD in {}", repo.display());
    }
    let head_sha = head_out.lines().next().unwrap_or("").trim().to_string();

    if head_sha == tag_sha {
        return Ok(Lineage::UpToDate { sha: head_sha });
    }

    let tag_in_head = is_ancestor(&repo_s, &tag_sha, "HEAD")?;
    let head_in_tag = is_ancestor(&repo_s, "HEAD", &tag_sha)?;

    // Counts are reported both ways: "how many commits would this add" and "how
    // many would it drop" are the two numbers an operator needs to judge a move.
    let add = commit_count(&repo_s, "HEAD", &tag_sha)?;
    let drop = commit_count(&repo_s, &tag_sha, "HEAD")?;

    Ok(if tag_in_head {
        Lineage::Rewind { add, drop }
    } else if head_in_tag {
        Lineage::FastForward { add, drop }
    } else {
        Lineage::Divergent { add, drop }
    })
}

/// `git describe --tags --always HEAD`, for messages that name the checkout.
pub fn describe(repo: &Path) -> String {
    let repo_s = repo.to_string_lossy().into_owned();
    env::run_capture(
        Path::new("git"),
        &["-C", &repo_s, "describe", "--tags", "--always", "HEAD"],
        None,
    )
    .map(|(_, out)| out.trim().to_string())
    .unwrap_or_else(|_| "(git describe failed)".to_string())
}

fn is_ancestor(repo: &str, ancestor: &str, descendant: &str) -> Result<bool> {
    let (code, out) = env::run_capture(
        Path::new("git"),
        &[
            "-C",
            repo,
            "merge-base",
            "--is-ancestor",
            ancestor,
            descendant,
        ],
        None,
    )?;
    match code {
        0 => Ok(true),
        1 => Ok(false),
        other => bail!(
            "git merge-base --is-ancestor exited {}: {}",
            other,
            out.trim()
        ),
    }
}

/// Commits reachable from `to` but not from `from` (`git rev-list --count from..to`).
fn commit_count(repo: &str, from: &str, to: &str) -> Result<u64> {
    let range = format!("{from}..{to}");
    let (out, _) = env::run_checked(
        Path::new("git"),
        &["-C", repo, "rev-list", "--count", &range],
        None,
    )
    .with_context(|| format!("count commits in {range}"))?;
    out.trim()
        .parse::<u64>()
        .with_context(|| format!("git printed a non-number for {range}: {}", out.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn repo(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("hug-lineage-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "one"]);
        dir
    }

    #[test]
    fn parses_the_documented_check_line() {
        let out = "Selected release available: stable v0.22.7 (088f657ecf)\n";
        assert_eq!(parse_target_tag(out).as_deref(), Some("v0.22.7"));
        let older = "Update available: canary 20261029T064840Z (abc1234)\n";
        assert_eq!(parse_target_tag(older).as_deref(), None);
    }

    #[test]
    fn no_version_token_means_no_tag() {
        assert_eq!(parse_target_tag("Up to date\n"), None);
        assert_eq!(
            parse_target_tag("Selected release available: stable\n"),
            None
        );
    }

    #[test]
    fn forward_rewind_and_divergent_are_judged_from_one_repo() {
        let dir = repo("verdicts");
        let first = git(&dir, &["rev-parse", "HEAD"]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "two"]);
        let second = git(&dir, &["rev-parse", "HEAD"]);
        git(&dir, &["tag", "v2", &second]);
        git(&dir, &["tag", "v1", &first]);

        // HEAD == tag: no move at all.
        assert_eq!(
            classify(&dir, "v2").unwrap(),
            Lineage::UpToDate {
                sha: second.clone()
            }
        );

        // A tag behind HEAD is a rewind, however many commits it is short.
        assert_eq!(
            classify(&dir, "v1").unwrap(),
            Lineage::Rewind { add: 0, drop: 1 }
        );

        // From the first commit, v2 is a clean fast-forward.
        git(&dir, &["checkout", "-q", "--detach", &first]);
        assert_eq!(
            classify(&dir, "v2").unwrap(),
            Lineage::FastForward { add: 1, drop: 0 }
        );

        // A fork off `first` is neither direction.
        git(&dir, &["checkout", "-q", "-B", "side", &first]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "side"]);
        git(&dir, &["tag", "vside"]);
        let side = git(&dir, &["rev-parse", "HEAD"]);
        git(&dir, &["checkout", "-q", "--detach", &second]);
        assert_eq!(
            classify(&dir, "vside").unwrap(),
            Lineage::Divergent { add: 1, drop: 1 },
            "side sha {side} and main sha {second} never meet"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unfetched_tag_is_reported_as_missing_not_as_a_pass() {
        let dir = repo("missing");
        assert_eq!(classify(&dir, "v9.9.9").unwrap(), Lineage::TagMissing);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
