use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Last-resort profile list, used only when `profiles/` cannot be read.
/// Deliberately generic: this binary ships publicly, so it must not carry one
/// operator's profile names, and it must not invent profiles that do not exist.
pub const PROFILES: [&str; 1] = ["default"];

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
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            // `.deleted` is Hermes' tombstone directory, not a profile
            .filter(|n| !n.starts_with('.'))
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

/// Profiles that were present in the newest pre-update backup but are ABSENT now.
///
/// Operator-agnostic on purpose: the public binary carries no profile names, so
/// this compares the home against the guard's own history instead of a hardcoded
/// list. A profile that disappears between two updates (deleted, half-restored,
/// moved aside) used to vanish from the fleet silently — it simply stopped
/// appearing — and every command still reported success.
pub fn profiles_gone_since_backup(home: &Path) -> Vec<String> {
    let backups = home.join("backups");
    let Ok(rd) = std::fs::read_dir(&backups) else {
        return Vec::new();
    };
    let mut dirs: Vec<(String, PathBuf)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?.to_string();
            name.starts_with("pre-update-").then_some((name, p))
        })
        .collect();
    dirs.sort();

    // Newest stamp per profile, across EVERY backup — not just the latest one.
    // A profile can disappear between two updates, and if it is already gone from
    // the newest backup the latest-only comparison never sees it.
    let mut last_seen: std::collections::BTreeMap<String, String> = Default::default();
    for (stamp, dir) in &dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if !(p.is_dir() && (p.join("state.db").exists() || p.join("config.yaml").exists())) {
                continue;
            }
            let Some(name) = e.file_name().to_str().map(|s| s.to_string()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            last_seen.insert(name, stamp.clone()); // sorted, so this ends at the newest
        }
    }

    let now = profiles(home);
    last_seen
        .into_iter()
        .filter(|(n, _)| !now.iter().any(|x| x == n))
        .map(|(n, s)| format!("{} (last seen {})", n, s.trim_start_matches("pre-update-")))
        .collect()
}

/// Profile directories that exist but have NO `config.yaml`.
///
/// A directory-only filter used to decide the fleet, so a half-deleted or
/// half-restored profile vanished from it: nobody backed it up, nobody verified
/// it, and every command still reported success. Those profiles are now part of
/// the fleet (their `state.db` still gets backed up) and reported here.
pub fn profiles_without_config(home: &Path) -> Vec<String> {
    profiles(home)
        .into_iter()
        .filter(|p| !config_yaml(home, p).is_file())
        .collect()
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

/// Free space on the volume that holds `path`, in bytes.
///
/// The fleet's state.db files are large (one profile was measured at 2.0 GB and the
/// whole fleet at ~2.9 GB), so a backup can fill a disk. The gate needs real
/// numbers, not a guess: `df -Pk` on POSIX, and the Windows shell's drive query.
pub fn free_space_bytes(path: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        let drive = path
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .unwrap_or_else(|| "C:".into());
        let letter = drive.trim_end_matches(['\\', '/']).trim_end_matches(':');
        if letter.is_empty() {
            return None;
        }
        let script = format!("(Get-PSDrive -Name {}).Free", letter);
        let (code, out) = run_capture(
            Path::new("powershell"),
            &["-NoProfile", "-Command", &script],
            None,
        )
        .ok()?;
        if code != 0 {
            return None;
        }
        out.trim().parse::<f64>().ok().map(|f| f as u64)
    }
    #[cfg(not(windows))]
    {
        let (code, out) =
            run_capture(Path::new("df"), &["-Pk", &path.to_string_lossy()], None).ok()?;
        if code != 0 {
            return None;
        }
        // df -Pk: Filesystem 1024-blocks Used Available Capacity Mounted
        let line = out.lines().nth(1)?;
        let avail_kb: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
        Some(avail_kb * 1024)
    }
}

/// Sum of the sizes of the files a backup would copy.
pub fn dir_size_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(md) = std::fs::symlink_metadata(path) {
        if md.is_file() {
            return md.len();
        }
    }
    if let Ok(rd) = std::fs::read_dir(path) {
        for e in rd.flatten() {
            total += dir_size_bytes(&e.path());
        }
    }
    total
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

/// Tri-state answer to "is this process running?".
///
/// `Unknown` is NOT `Absent`. The guard's contract is that a check which cannot
/// run must never read as a pass: a probe that fails open lets an update start
/// while the Desktop app still holds every profile's state.db. Callers must
/// treat `Unknown` as `Running` and refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcState {
    Running,
    Absent,
    Unknown(String),
}

impl ProcState {
    pub fn is_running(&self) -> bool {
        matches!(self, ProcState::Running)
    }
}

/// Rows of `tasklist /FO CSV /NH`, as `(image, pid)`. Anything that is not a
/// quoted CSV row (headers, headers in any locale, prose) is skipped.
///
/// Windows-only in production (POSIX uses `kill -0`), but kept compiled
/// everywhere so the parser is testable on any host. Without the attribute,
/// `clippy -D warnings` fails with `dead_code` on Linux/macOS and the whole CI
/// matrix goes red.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn csv_rows(out: &str) -> Vec<(String, u32)> {
    out.lines()
        .filter_map(|line| {
            let line = line.trim();
            if !line.starts_with('"') {
                return None;
            }
            let mut parts = line.trim_matches('"').split("\",\"");
            let name = parts.next()?.trim_matches('"').to_string();
            let pid = parts.next()?.trim_matches('"').parse::<u32>().ok()?;
            Some((name, pid))
        })
        .collect()
}

/// PIDs whose image name equals `image` (case-insensitive, as Windows itself
/// compares image names). Pure, so the CSV shape is testable without Windows.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn parse_tasklist_csv(out: &str, image: &str) -> Vec<u32> {
    csv_rows(out)
        .into_iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(image))
        .map(|(_, pid)| pid)
        .collect()
}

/// PIDs of every process with this image name. Evidence only: this is never
/// used to decide whether an update may proceed (see [`process_state`]).
#[cfg(windows)]
pub fn process_pids(image: &str) -> Vec<u32> {
    let filter = format!("IMAGENAME eq {}", image);
    match run_capture(
        Path::new("tasklist"),
        &["/FI", filter.as_str(), "/FO", "CSV", "/NH"],
        None,
    ) {
        Ok((0, out)) => parse_tasklist_csv(&out, image),
        _ => Vec::new(),
    }
}

#[cfg(not(windows))]
pub fn process_pids(image: &str) -> Vec<u32> {
    match run_capture(Path::new("pgrep"), &["-f", image], None) {
        Ok((0, out)) => out
            .lines()
            .filter_map(|l| l.trim().parse::<u32>().ok())
            .collect(),
        _ => Vec::new(),
    }
}

/// Fail-closed process probe. A spawn failure, a non-zero exit or output we do
/// not recognise is `Unknown`, never `Absent` (fixed 2026-10-19: the old
/// `process_running -> bool` answered `false` on every error, so a broken
/// `tasklist` read as "the Desktop app is closed").
#[cfg(windows)]
pub fn process_state(image: &str) -> ProcState {
    let filter = format!("IMAGENAME eq {}", image);
    match run_capture(
        Path::new("tasklist"),
        &["/FI", filter.as_str(), "/FO", "CSV", "/NH"],
        None,
    ) {
        Err(e) => ProcState::Unknown(format!("tasklist could not run: {e}")),
        Ok((code, out)) => {
            if code != 0 {
                // A bad filter or an access error: we genuinely do not know.
                return ProcState::Unknown(format!("tasklist exited {code}"));
            }
            // Exit 0 is the authority. A filter that matches nothing still exits 0
            // and prints a *localised* sentence ("no hay tareas...", "no tasks are
            // running..."), so the absence of a matching CSV row IS the answer.
            // Never match English prose here: this machine's Windows answers in
            // Spanish, and an English-only check turned "absent" into "unknown".
            if parse_tasklist_csv(&out, image).is_empty() {
                ProcState::Absent
            } else {
                ProcState::Running
            }
        }
    }
}

/// POSIX probe via pgrep -f (matches command lines, e.g. the Desktop app on macOS).
#[cfg(not(windows))]
pub fn process_state(image: &str) -> ProcState {
    match run_capture(Path::new("pgrep"), &["-f", image], None) {
        Err(e) => ProcState::Unknown(format!("pgrep could not run: {e}")),
        Ok((0, _)) => ProcState::Running,
        Ok((1, _)) => ProcState::Absent,
        Ok((code, out)) => ProcState::Unknown(format!("pgrep exited {code}: {}", out.trim())),
    }
}

/// Is `pid` still alive? Same tri-state discipline as the image probe: a failed
/// `tasklist` answers Unknown, never "dead".
#[cfg(windows)]
pub fn pid_running(pid: u32) -> ProcState {
    let filter = format!("PID eq {}", pid);
    match run_capture(
        Path::new("tasklist"),
        &["/FI", filter.as_str(), "/FO", "CSV", "/NH"],
        None,
    ) {
        Err(e) => ProcState::Unknown(format!("tasklist could not run: {e}")),
        Ok((code, out)) => {
            if code != 0 {
                return ProcState::Unknown(format!("tasklist exited {code}"));
            }
            if csv_rows(&out).iter().any(|(_, p)| *p == pid) {
                ProcState::Running
            } else {
                ProcState::Absent
            }
        }
    }
}

#[cfg(not(windows))]
pub fn pid_running(pid: u32) -> ProcState {
    match run_capture(Path::new("kill"), &["-0", &pid.to_string()], None) {
        Ok((0, _)) => ProcState::Running,
        Ok((1, _)) => ProcState::Absent,
        Ok((code, _)) => ProcState::Unknown(format!("kill -0 exited {code}")),
        Err(e) => ProcState::Unknown(format!("kill could not run: {e}")),
    }
}

/// Ask the OS to close every process with this image. `force = false` is a
/// graceful request (WM_CLOSE on Windows, SIGTERM on POSIX).
#[cfg(windows)]
fn request_close(image: &str, force: bool) -> Result<()> {
    let mut args = vec!["/IM", image];
    if force {
        args.push("/F");
    }
    args.push("/T");
    let (code, out) = run_capture(Path::new("taskkill"), &args, None)?;
    if code != 0 {
        bail!("taskkill exited {code}: {}", out.trim());
    }
    Ok(())
}

#[cfg(not(windows))]
fn request_close(image: &str, force: bool) -> Result<()> {
    let mut args = vec!["-f", image];
    if force {
        args.push("-9");
    }
    let (code, out) = run_capture(Path::new("pkill"), &args, None)?;
    // pkill exits 1 when nothing matched, which is not an error for our purpose.
    if code != 0 && code != 1 {
        bail!("pkill exited {code}: {}", out.trim());
    }
    Ok(())
}

/// Poll `probe` until it stops reporting `Running`, or `timeout` elapses.
/// `Unknown` ends the wait immediately: continuing would mean guessing.
pub fn wait_gone<F: FnMut() -> ProcState>(
    mut probe: F,
    timeout: std::time::Duration,
    step: std::time::Duration,
) -> ProcState {
    let start = std::time::Instant::now();
    loop {
        let state = probe();
        if !state.is_running() {
            return state;
        }
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return state;
        }
        let remaining = timeout - elapsed;
        std::thread::sleep(if step < remaining { step } else { remaining });
    }
}

/// Outcome of a close attempt: `closed = false` carries the surviving PIDs so the
/// operator gets evidence instead of "still running".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseReport {
    pub closed: bool,
    pub detail: String,
    pub survivors: Vec<u32>,
}

/// Close the app in two bounded stages: graceful, then forced. The old code sent
/// one forced kill and slept a fixed 6 s, so a slow shutdown (or an app that
/// respawns a helper) failed the whole update with no diagnosis.
pub fn close_process(
    image: &str,
    graceful: std::time::Duration,
    forced: std::time::Duration,
) -> Result<CloseReport> {
    match process_state(image) {
        ProcState::Absent => {
            return Ok(CloseReport {
                closed: true,
                detail: "already closed".into(),
                survivors: Vec::new(),
            })
        }
        ProcState::Unknown(why) => {
            bail!("cannot verify whether {image} is running ({why}) - treating it as running")
        }
        ProcState::Running => {}
    }

    let step = std::time::Duration::from_millis(250);
    let _ = request_close(image, false);
    match wait_gone(|| process_state(image), graceful, step) {
        ProcState::Absent => {
            return Ok(CloseReport {
                closed: true,
                detail: format!("closed gracefully within {}s", graceful.as_secs()),
                survivors: Vec::new(),
            })
        }
        ProcState::Unknown(why) => bail!("cannot verify {image} after a graceful close ({why})"),
        ProcState::Running => {}
    }

    let _ = request_close(image, true);
    match wait_gone(|| process_state(image), forced, step) {
        ProcState::Absent => Ok(CloseReport {
            closed: true,
            detail: format!("closed after a forced stop within {}s", forced.as_secs()),
            survivors: Vec::new(),
        }),
        ProcState::Running => {
            let survivors = process_pids(image);
            Ok(CloseReport {
                closed: false,
                detail: format!(
                    "still running after graceful ({}s) + forced ({}s) close",
                    graceful.as_secs(),
                    forced.as_secs()
                ),
                survivors,
            })
        }
        ProcState::Unknown(why) => bail!("cannot verify {image} after a forced stop ({why})"),
    }
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

    /// Windows image names are case-insensitive, so `Hermes.exe` and `hermes.exe`
    /// are the same image and `tasklist`/`taskkill` both match all of them. Pin
    /// that, because the guard's survivors list is read by a human.
    #[test]
    fn parse_tasklist_csv_matches_image_case_insensitively() {
        let out = "\"Hermes.exe\",\"18068\",\"Console\",\"1\",\"145.268 KB\"\r\n\
                   \"hermes.exe\",\"13044\",\"Console\",\"1\",\"4.276 KB\"\r\n";
        assert_eq!(parse_tasklist_csv(out, "Hermes.exe"), vec![18068, 13044]);
        assert_eq!(parse_tasklist_csv(out, "HERMES.EXE"), vec![18068, 13044]);
    }

    /// Exit 0 with no matching row means ABSENT, in any locale: a Spanish Windows
    /// prints "no hay tareas ejecutandose...", which an English-only check would
    /// read as "output not understood".
    #[test]
    fn csv_rows_ignores_localised_prose() {
        let es = "INFORMACION: no hay tareas ejecutandose que coincidan con los criterios especificados.\r\n";
        assert!(csv_rows(es).is_empty());
        assert!(parse_tasklist_csv(es, "Hermes.exe").is_empty());
    }

    /// Exact match, never a substring: a helper image must not be counted as the
    /// app itself, or a "closed" verdict would be judged against the wrong rows.
    #[test]
    fn parse_tasklist_csv_is_not_a_substring_match() {
        let out = "\"Hermes Helper.exe\",\"999\",\"Console\",\"1\",\"1 KB\"\r\n";
        assert!(parse_tasklist_csv(out, "Hermes.exe").is_empty());
    }

    /// "No tasks are running" is prose, not a row: the parser must return empty
    /// and `process_state` must read that as Absent, never as a PID list.
    #[test]
    fn parse_tasklist_csv_ignores_prose_and_header() {
        let out = "INFO: No tasks are running which match the specified criteria.\r\n";
        assert!(parse_tasklist_csv(out, "Hermes.exe").is_empty());
    }

    /// A probe that stops answering must end the wait at once: waiting cannot
    /// turn `Unknown` into `Absent`.
    #[test]
    fn wait_gone_stops_on_unknown() {
        let mut calls = 0;
        let state = wait_gone(
            || {
                calls += 1;
                ProcState::Unknown("tasklist broke".into())
            },
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(1),
        );
        assert_eq!(calls, 1, "an unknown probe must not be polled again");
        assert!(matches!(state, ProcState::Unknown(_)), "got {state:?}");
    }

    /// A process that never leaves must be reported as Running at the deadline,
    /// not silently as gone.
    #[test]
    fn wait_gone_times_out_running() {
        let state = wait_gone(
            || ProcState::Running,
            std::time::Duration::from_millis(30),
            std::time::Duration::from_millis(5),
        );
        assert_eq!(state, ProcState::Running);
    }

    /// A process that leaves between polls must be reported as Absent.
    #[test]
    fn wait_gone_sees_the_exit() {
        let mut calls = 0;
        let state = wait_gone(
            || {
                calls += 1;
                if calls < 2 {
                    ProcState::Running
                } else {
                    ProcState::Absent
                }
            },
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(1),
        );
        assert_eq!(state, ProcState::Absent);
    }

    /// A profile directory without config.yaml must stay in the fleet (its
    /// state.db still needs backing up) and be reported as an anomaly. Dropping
    /// it made a half-deleted profile invisible to backup and verify alike.
    #[test]
    fn profiles_include_dirs_without_config_and_flag_them() {
        let root = std::env::temp_dir().join(format!("hug-fleet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("profiles/alpha")).unwrap();
        std::fs::create_dir_all(root.join("profiles/beta")).unwrap();
        std::fs::create_dir_all(root.join("profiles/.deleted")).unwrap();
        std::fs::write(root.join("profiles/alpha/config.yaml"), "model: {}\n").unwrap();

        let fleet = profiles(&root);
        assert_eq!(
            fleet,
            vec!["alpha".to_string(), "beta".to_string()],
            ".deleted excluded, beta kept despite no config"
        );
        assert_eq!(profiles_without_config(&root), vec!["beta".to_string()]);
        // no root config.yaml => no "default" entry
        assert!(!fleet.contains(&"default".to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A profile present in the last backup but gone now must be reported, and
    /// a home with no backup history must stay quiet (no false alarms).
    #[test]
    fn missing_profile_detected_from_backup_history() {
        let home = std::env::temp_dir().join(format!("hug-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("profiles/mcpvot")).unwrap();
        std::fs::write(home.join("config.yaml"), "model: {}\n").unwrap();

        let bk = home.join("backups/pre-update-20260101-000000");
        for p in ["default", "elevate", "mcpvot"] {
            std::fs::create_dir_all(bk.join(p)).unwrap();
            std::fs::write(bk.join(p).join("state.db"), "x").unwrap();
        }
        // newest backup wins: an older one must not be consulted
        let old = home.join("backups/pre-update-20250101-000000");
        std::fs::create_dir_all(old.join("ancient")).unwrap();
        std::fs::write(old.join("ancient/state.db"), "x").unwrap();

        // elevate appears in BOTH backups: the report must carry the NEWEST stamp
        std::fs::create_dir_all(old.join("elevate")).unwrap();
        std::fs::write(old.join("elevate/state.db"), "x").unwrap();

        assert_eq!(
            profiles_gone_since_backup(&home),
            vec![
                "ancient (last seen 20250101-000000)".to_string(),
                "elevate (last seen 20260101-000000)".to_string(),
            ],
            "every backup is scanned, and the newest sighting wins"
        );

        // no backups at all -> nothing claimed
        let bare = std::env::temp_dir().join(format!("hug-bare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&bare);
        std::fs::create_dir_all(&bare).unwrap();
        assert!(profiles_gone_since_backup(&bare).is_empty());

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&bare);
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
