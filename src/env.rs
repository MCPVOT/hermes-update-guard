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

/// Enumerate the profiles that actually exist in this home.
///
/// `PROFILES` is only a fallback. The guard runs on machines whose profile set is
/// different (the droplet runs hoodbot/sentinel/solana/storemoat), and a hardcoded
/// fleet there silently backs up and verifies the wrong things — or nothing at all.
pub fn profiles(home: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if home.join("config.yaml").is_file() {
        out.push("default".to_string());
    }
    if let Ok(rd) = std::fs::read_dir(home.join("profiles")) {
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("config.yaml").is_file())
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .collect();
        names.sort();
        names.dedup();
        out.extend(names);
    }
    if out.is_empty() {
        out = PROFILES.iter().map(|s| s.to_string()).collect();
    }
    out
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

/// Find the hermes CLI the way hermes itself does, instead of assuming a venv.
///
/// A venv-only assumption is wrong on every install that is not a source
/// checkout: the ops node runs `/usr/local/bin/hermes` against
/// `/usr/local/lib/hermes-agent`, and the droplet runs `~/.local/bin/hermes`.
/// Returning `None` (rather than a path that cannot exist) lets callers fail
/// closed with an honest message.
pub fn resolve_hermes(home: &Path) -> Option<std::path::PathBuf> {
    let venv = venv_hermes(home);
    if venv.is_file() {
        return Some(venv);
    }
    let bin = if cfg!(windows) {
        "hermes.exe"
    } else {
        "hermes"
    };
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    #[cfg(not(windows))]
    {
        if let Ok(h) = std::env::var("HOME") {
            candidates.push(PathBuf::from(h).join(".local").join("bin").join(bin));
        }
        candidates.push(PathBuf::from("/usr/local/bin").join(bin));
        candidates.push(PathBuf::from("/usr/bin").join(bin));
    }
    #[cfg(windows)]
    {
        if let Ok(la) = std::env::var("LOCALAPPDATA") {
            candidates.push(PathBuf::from(la).join("Programs").join("hermes").join(bin));
        }
    }
    for c in candidates {
        if c.is_file() {
            return Some(c);
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let c = dir.join(bin);
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

/// Run a command and treat a non-zero exit as an ERROR, returning trimmed stdout.
/// stdout and stderr are kept separate: mixing them is how a `fatal: cannot change
/// to ...` from git used to be counted as a "local modification" by verify.
pub fn run_checked(program: &Path, args: &[&str], cwd: Option<&Path>) -> Result<(String, String)> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    cmd.stdin(std::process::Stdio::null());
    let out = cmd
        .output()
        .with_context(|| format!("failed to spawn {}", program.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let code = out.status.code().unwrap_or(-1);
    if code != 0 {
        let detail = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        bail!(
            "{} {} exited {}: {}",
            program.display(),
            args.join(" "),
            code,
            truncated(&detail, 300)
        );
    }
    Ok((stdout, stderr))
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

/// Collect every process holding the hermes venv, via hermes' own probe.
///
/// `hermes update --list-venv-holders` prints
/// `[{"pid":..,"exe":..,"argv":[..],"kind":..}]` and exits **0 when the venv is
/// free / 3 when holders are present** (its own `--help` says so). Treating any
/// non-zero exit as "free" — which this function used to do — makes the guard
/// blind exactly when a session IS holding the venv, so exit 3 must be read as
/// data, not as an error.
///
/// Returns `(holders, exit_code, payload_was_json)`. When an update is already in
/// flight the CLI prints prose ("an update is still running; wait for it to
/// exit...") instead of the JSON list, so the third flag lets callers say
/// "venv state UNKNOWN — treat as held" rather than assuming free.
pub fn venv_holders_probe(home: &Path) -> (Vec<String>, i32, bool) {
    venv_holders_probe_with(resolve_hermes(home).as_deref())
}

/// Testable core of [`venv_holders_probe`]: `None` means no CLI was resolvable,
/// which must read as UNKNOWN (parsed = false), never as "venv free".
fn venv_holders_probe_with(cli: Option<&Path>) -> (Vec<String>, i32, bool) {
    let Some(hermes) = cli else {
        return (vec![], -1, false);
    };
    let Ok((code, out)) = run_capture(hermes, &["update", "--list-venv-holders"], None) else {
        return (vec![], -1, false);
    };
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return (vec![], code, true);
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::Array(arr)) if arr.is_empty() => (vec![], code, true),
        Ok(serde_json::Value::Array(arr)) => (arr.iter().map(format_holder).collect(), code, true),
        Ok(v @ serde_json::Value::Object(_)) => (vec![format_holder(&v)], code, true),
        Ok(_) => (
            vec![format!(
                "probe printed JSON that is neither a list nor an object: {}",
                truncated(trimmed, 160)
            )],
            code,
            false,
        ),
        Err(_) => (
            vec![format!(
                "probe printed {} non-empty bytes that are not the documented JSON: {}",
                trimmed.len(),
                truncated(trimmed, 160)
            )],
            code,
            false,
        ),
    }
}

fn format_holder(h: &serde_json::Value) -> String {
    let pid = h.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
    let kind = h.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
    let exe = h.get("exe").and_then(|v| v.as_str()).unwrap_or("?");
    let argv = h
        .get("argv")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    if argv.is_empty() {
        format!("pid={} kind={} exe={}", pid, kind, exe)
    } else {
        format!("pid={} kind={} {} {}", pid, kind, exe, truncated(&argv, 80))
    }
}

fn truncated(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push_str("...");
        out
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole fail-closed contract rests on run_checked: a non-zero exit must
    /// be an Err, never a silently-empty String that callers read as "clean".
    #[test]
    fn run_checked_treats_nonzero_exit_as_error() {
        let ok = run_checked(Path::new("git"), &["--version"], None);
        assert!(ok.is_ok(), "git --version must succeed");
        let bad = run_checked(
            Path::new("git"),
            &["rev-parse", "--verify", "refs/heads/definitely-not-a-ref"],
            None,
        );
        assert!(
            bad.is_err(),
            "a non-zero exit must be an error, not Ok(\"\")"
        );
    }

    /// A missing working directory must also be an error, not an empty stdout.
    #[test]
    fn run_checked_errors_on_missing_cwd() {
        let missing = Path::new("C:/definitely-not-here-xyz");
        assert!(run_checked(Path::new("git"), &["status"], Some(missing)).is_err());
    }

    /// venv_holders_probe must report "unknown" (parsed=false) when there is no
    /// hermes CLI at all, so callers fail closed instead of assuming the venv free.
    #[test]
    fn venv_probe_unknown_without_hermes() {
        let (holders, code, parsed) = venv_holders_probe_with(None);
        assert!(!parsed, "no hermes CLI must mean UNKNOWN, not free");
        assert_eq!(code, -1);
        assert!(holders.is_empty());
    }

    /// An unparseable answer (hermes printing prose while an update is in flight)
    /// must also read as UNKNOWN rather than being dropped on the floor.
    #[test]
    fn venv_probe_unknown_on_prose() {
        // any CLI that prints prose instead of the documented JSON
        let (_, code, parsed) = venv_holders_probe_with(Some(Path::new("git")));
        assert!(!parsed, "prose output is not the documented JSON");
        assert_ne!(code, 0, "git has no `update` subcommand");
    }
}
