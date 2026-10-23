//! `selfcheck` — is the guard I am actually running the one I think I am?
//!
//! WHY THIS EXISTS
//!   The 2026-10-19 audit's first finding was: "the tool guarding it was not the one
//!   you were running" — a STALE v0.1.1 binary was on PATH while the repo was at
//!   v0.3.0, and because `~/.cargo/bin` was not on PATH, `hermes-update-guard
//!   preflight` did nothing at all. A stale guard is worse than no guard: it
//!   reports a clean preflight for a version that no longer exists.
//!
//! WHAT IT CHECKS
//!   1. which `hermes-update-guard` resolves on PATH, and what version it is
//!   2. other copies/clones of the tool under the Hermes home (a `cargo install`
//!      run from one of those reinstalls the stale build)
//!   3. when a checkout is pointed at (--repo / $HERMES_GUARD_REPO): its version,
//!      git HEAD, dirty state, and whether the INSTALLED binary is older than the
//!      repo's own build
//!   4. the Hermes home, the discovered fleet, and the context-health verdict
//!
//! Read-only. Exit 1 when something needs attention.

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::env;

/// Locate the binary that would run for `hermes-update-guard` on PATH.
fn which_guard() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exe = if cfg!(windows) {
        "hermes-update-guard.exe"
    } else {
        "hermes-update-guard"
    };
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let cand = dir.join(exe);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

fn version_of(bin: &Path) -> String {
    env::run_capture(bin, &["--version"], None)
        .map(|(_, out)| out.trim().to_string())
        .unwrap_or_else(|_| "?".into())
}

fn mtime_of(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

fn read_repo_version(repo: &Path) -> Option<String> {
    let text = std::fs::read_to_string(repo.join("Cargo.toml")).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("version") {
            if let Some(v) = rest.trim_start().strip_prefix('=') {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

/// Run the self-check. Exits 1 when something needs attention.
pub fn run(home: &Path, repo: Option<&Path>) -> Result<()> {
    println!("== GUARD SELF-CHECK ==");
    let mut problems = false;

    // 1. which binary is actually on PATH
    let on_path = which_guard();
    match &on_path {
        Some(bin) => {
            let v = version_of(bin);
            println!("  on PATH     : {}", bin.display());
            println!("  version     : {}", v);
            let m = mtime_of(bin)
                .map(|t| {
                    let secs = t
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    chrono::DateTime::from_timestamp(secs as i64, 0)
                        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "?".into())
                })
                .unwrap_or_else(|| "?".into());
            println!("  built       : {}", m);
            println!("  this build  : {}", env!("CARGO_PKG_VERSION"));
            if !v.contains(env!("CARGO_PKG_VERSION")) {
                println!("  ! the binary on PATH reports a DIFFERENT version than this build");
                problems = true;
            }
        }
        None => {
            println!("  on PATH     : (not found - `hermes-update-guard` will not resolve)");
            problems = true;
        }
    }

    // 2. other copies under the Hermes home (a `cargo install` from one of these
    //    reinstalls a stale build)
    let tools = home.join("tools");
    if tools.is_dir() {
        let mut clones = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&tools) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.contains("hermes-update-guard") {
                    clones.push(name);
                }
            }
        }
        if clones.is_empty() {
            println!("  stale clones: none under {}\\tools", home.display());
        } else {
            println!(
                "  ! {} copy/copies of the tool under {}\\tools (a `cargo install` from one reinstalls a stale build): {}",
                clones.len(),
                home.display(),
                clones.join(", ")
            );
            problems = true;
        }
    }

    // 3. compare against a checkout, when one is pointed at
    let repo_path: Option<PathBuf> = repo
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::var_os("HERMES_GUARD_REPO").map(PathBuf::from));
    if let Some(rp) = repo_path {
        if rp.is_dir() {
            println!("  repo        : {}", rp.display());
            // A stale checkout BESIDE the live one is the same hazard as one under
            // <home>\\tools, and scanning only that directory printed "no stale
            // clones" while a v0.1.1 checkout sat in the same parent folder. This is
            // outside the Hermes home, so it is reported, not counted as a problem.
            if let Some(parent) = rp.parent() {
                let me = rp
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let mut siblings: Vec<String> = Vec::new();
                if let Ok(rd) = std::fs::read_dir(parent) {
                    for e in rd.flatten() {
                        let n = e.file_name().to_string_lossy().to_string();
                        if n != me && n.contains("hermes-update-guard") && e.path().is_dir() {
                            let v = read_repo_version(&e.path()).unwrap_or_else(|| "?".into());
                            siblings.push(format!("{n} (v{v})"));
                        }
                    }
                }
                siblings.sort();
                if !siblings.is_empty() {
                    println!(
                        "  ! {} other checkout(s) beside this repo - a `cargo install --path` from one installs THAT build: {}",
                        siblings.len(),
                        siblings.join(", ")
                    );
                }
            }
            if let Some(v) = read_repo_version(&rp) {
                println!("  repo version: {}", v);
                if v != env!("CARGO_PKG_VERSION") {
                    println!(
                        "  ! repo version differs from this build ({})",
                        env!("CARGO_PKG_VERSION")
                    );
                    problems = true;
                }
            }
            let (code, head) = env::run_capture(
                Path::new("git"),
                &[
                    "-C",
                    rp.to_str().unwrap_or("."),
                    "rev-parse",
                    "--short",
                    "HEAD",
                ],
                None,
            )
            .unwrap_or((-1, "?".into()));
            if code == 0 {
                println!("  repo HEAD   : {}", head.trim());
            }
            let (code, dirty) = env::run_capture(
                Path::new("git"),
                &["-C", rp.to_str().unwrap_or("."), "status", "--porcelain"],
                None,
            )
            .unwrap_or((-1, String::new()));
            if code == 0 && !dirty.trim().is_empty() {
                println!(
                    "  ! repo has uncommitted changes ({} line(s)) - the installed binary may predate them",
                    dirty.trim().lines().count()
                );
                problems = true;
            }
            // Is the INSTALLED binary older than the repo's own build?
            let repo_bin = if cfg!(windows) {
                rp.join("target")
                    .join("release")
                    .join("hermes-update-guard.exe")
            } else {
                rp.join("target")
                    .join("release")
                    .join("hermes-update-guard")
            };
            if repo_bin.is_file() {
                if let (Some(inst), Some(rep)) = (
                    on_path.as_ref().and_then(|b| mtime_of(b)),
                    mtime_of(&repo_bin),
                ) {
                    if rep > inst {
                        println!(
                            "  ! the repo's build is NEWER than the installed binary - reinstall before trusting a preflight"
                        );
                        problems = true;
                    } else {
                        println!("  installed binary is at least as new as the repo build");
                    }
                }
            }
        } else {
            println!(
                "  repo        : {} (not a directory - ignored)",
                rp.display()
            );
        }
    } else {
        println!("  repo        : (not set - pass --repo or set HERMES_GUARD_REPO to compare against a checkout)");
    }

    // 4. the fleet and the health verdict
    println!("  hermes home : {}", home.display());
    let fleet = env::profiles(home);
    println!(
        "  fleet       : {} ({} profiles)",
        fleet.join(", "),
        fleet.len()
    );
    let missing = env::profiles_gone_since_backup(home, &crate::retire::retired(home));
    if !missing.is_empty() {
        println!(
            "  ! profile(s) present in an earlier backup but ABSENT now: {} - deleted, or never restored (recover from backups/pre-update-*)",
            missing.join(", ")
        );
        problems = true;
    }
    let no_cfg = env::profiles_without_config(home);
    if !no_cfg.is_empty() {
        println!(
            "  ! {} profile dir(s) have NO config.yaml: {} - no drift protection, and a half-restored profile looks like this",
            no_cfg.len(),
            no_cfg.join(", ")
        );
        problems = true;
    }
    println!();
    if crate::context_health::report(home)? {
        problems = true;
    }

    if problems {
        println!("\n  [FAIL] self-check found something to fix");
        std::process::exit(1);
    }
    println!("\n  [ok]   self-check clean");
    Ok(())
}
