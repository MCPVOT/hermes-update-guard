use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The static profile fleet this machine runs (matches update-hermes-v2.ps1).
pub const PROFILES: [&str; 7] = [
    "default",
    "elevate",
    "mcpvot",
    "ofcrm",
    "example",
    "polloriko",
    "vot",
];

/// Resolve the Hermes home the same way the PS1 scripts do.
///
/// In a live Hermes session HERMES_HOME is often the *active profile* dir
/// (e.g. .../hermes/profiles/elevate), which is NOT the root where
/// hermes-agent/, profiles/ and backups/ live. We therefore only accept
/// HERMES_HOME when it structurally looks like a root home; otherwise fall
/// back to the platform default and let callers pass --home explicitly.
pub fn hermes_home() -> Result<PathBuf> {
    if let Ok(h) = std::env::var("HERMES_HOME") {
        let p = PathBuf::from(h);
        if looks_like_home(&p) {
            return Ok(p);
        }
    }
    default_home()
}

/// --home-style explicit root (validated in main before use).
pub fn hermes_home_from(p: &Path) -> Result<PathBuf> {
    if !looks_like_home(p) {
        bail!(
            "{} does not look like a hermes home (no hermes-agent/ and no profiles/)",
            p.display()
        );
    }
    Ok(p.to_path_buf())
}

fn looks_like_home(p: &Path) -> bool {
    // A root hermes home structurally contains hermes-agent/ and/or profiles/.
    // (A PROFILE dir also has config.yaml, so that check alone is NOT sufficient.)
    p.is_dir() && (p.join("hermes-agent").is_dir() || p.join("profiles").is_dir())
}

fn default_home() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let la = std::env::var("LOCALAPPDATA").context("LOCALAPPDATA not set")?;
        Ok(PathBuf::from(la).join("hermes"))
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        Ok(PathBuf::from(home).join(".hermes"))
    }
}

/// Profile home directory: default lives at hermes home root, others under profiles/.
pub fn profile_dir(home: &Path, profile: &str) -> PathBuf {
    if profile == "default" {
        home.to_path_buf()
    } else {
        home.join("profiles").join(profile)
    }
}

pub fn state_db(home: &Path, profile: &str) -> PathBuf {
    profile_dir(home, profile).join("state.db")
}

pub fn config_yaml(home: &Path, profile: &str) -> PathBuf {
    profile_dir(home, profile).join("config.yaml")
}

#[cfg(windows)]
pub fn venv_hermes(home: &Path) -> PathBuf {
    home.join("hermes-agent")
        .join("venv")
        .join("Scripts")
        .join("hermes.exe")
}

#[cfg(not(windows))]
pub fn venv_hermes(home: &Path) -> PathBuf {
    home.join("hermes-agent")
        .join("venv")
        .join("bin")
        .join("hermes")
}

/// Capture stdout+stderr of a command (no stdin). Returns (exit_code, combined_output).
pub fn run_capture(program: &Path, args: &[&str], cwd: Option<&Path>) -> Result<(i32, String)> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    cmd.stdin(std::process::Stdio::null());
    let out = cmd
        .output()
        .with_context(|| format!("failed to spawn {}", program.display()))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.stderr.is_empty() {
        text.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    Ok((out.status.code().unwrap_or(-1), text))
}

/// Collect all commands under `name` that hold the hermes venv (via hermes' own list).
pub fn venv_holders(home: &Path) -> Vec<String> {
    let hermes = venv_hermes(home);
    let Ok((code, out)) = run_capture(&hermes, &["update", "--list-venv-holders"], None) else {
        return Vec::new();
    };
    if code != 0 {
        return Vec::new();
    }
    let trimmed = out.trim();
    if trimmed == "[]" || trimmed.is_empty() {
        return Vec::new();
    }
    trimmed
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Simple "anything is running named like this on Windows" probe.
/// Returns true when at least one process of `image` exists.
#[cfg(windows)]
pub fn process_running(image: &str) -> bool {
    let Ok((code, out)) = run_capture(
        Path::new("tasklist"),
        &["/FI", &format!("IMAGENAME eq {}", image)],
        None,
    ) else {
        return false;
    };
    if code != 0 {
        return false;
    }
    out.to_lowercase().contains(&image.to_lowercase())
}

/// POSIX probe via pgrep -f (matches command lines, e.g. "Hermes" desktop app on macOS
/// or a `hermes` python process on Linux).
#[cfg(not(windows))]
pub fn process_running(image: &str) -> bool {
    let Ok((code, _out)) = run_capture(Path::new("pgrep"), &["-f", image], None) else {
        return false;
    };
    code == 0
}

/// Force-stop every process matching `image` (taskkill on Windows, pkill on POSIX).
/// Returns true when the kill command itself exited 0.
#[cfg(windows)]
pub fn kill_process(image: &str) -> bool {
    matches!(
        run_capture(Path::new("taskkill"), &["/IM", image, "/F", "/T"], None),
        Ok((0, _))
    )
}

#[cfg(not(windows))]
pub fn kill_process(image: &str) -> bool {
    matches!(
        run_capture(Path::new("pkill"), &["-f", image], None),
        Ok((0, _))
    )
}
