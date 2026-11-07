use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
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
pub fn profiles_gone_since_backup(
    home: &Path,
    retired: &std::collections::BTreeSet<String>,
    bulk_root: &Path,
) -> Vec<String> {
    let now = profiles(home);
    last_seen_in_backups(bulk_root)
        .into_iter()
        .filter(|(n, _)| !now.iter().any(|x| x == n) && !retired.contains(n))
        .map(|(n, s)| format!("{} (last seen {})", n, s.trim_start_matches("pre-update-")))
        .collect()
}

/// Newest `pre-update-*` backup stamp in which each profile directory appears.
///
/// A profile can disappear between two updates, and if it is already gone from the
/// newest backup a latest-only comparison never sees it, so every backup is scanned.
///
/// `bulk_root` is the RESOLVED bulk root, not `<home>/backups`: the restore points
/// may live on a mounted disk, and this question is about those runs.
pub fn last_seen_in_backups(bulk_root: &Path) -> std::collections::BTreeMap<String, String> {
    let Ok(rd) = std::fs::read_dir(bulk_root) else {
        return Default::default();
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

    last_seen
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

/// The ALWAYS-LOCAL guard root: run state, the log, provider-surface baselines,
/// log-signal snapshots and quarantine all live here.
///
/// It is deliberately not configurable. `watch`, `verify`, `self-check` and the
/// drift baseline must keep working when the removable disk is detached — which
/// is exactly the moment a post-update verify matters.
pub fn control_root(home: &Path) -> PathBuf {
    home.join("backups")
}

/// Where `pre-update-<stamp>/` directories are written.
///
/// Precedence: `--backup-root`, then `$HERMES_GUARD_BULK_ROOT`, else the control
/// root (today's behaviour, byte-for-byte). The path is NOT validated here; the
/// mount proof belongs to the commands that write, so a detached disk cannot
/// stop `verify` from reading the control root.
pub fn bulk_root(home: &Path, cli: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = cli {
        return Ok(p.to_path_buf());
    }
    if let Ok(v) = std::env::var("HERMES_GUARD_BULK_ROOT") {
        if !v.trim().is_empty() {
            return Ok(PathBuf::from(v));
        }
    }
    Ok(control_root(home))
}

/// What the guard can PROVE about the volume it is about to write to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetProof {
    pub path: PathBuf,
    /// Kernel source device (`/dev/sda2`), or `local` for the control root.
    pub device: String,
    pub fstype: String,
    pub is_mountpoint: bool,
    pub free_bytes: Option<u64>,
}

impl TargetProof {
    pub fn print(&self) {
        println!("  target : {}", self.path.display());
        println!(
            "  device : {}",
            if self.device.is_empty() {
                "(unknown)"
            } else {
                self.device.as_str()
            }
        );
        println!("  fstype : {}", self.fstype);
        println!("  mount  : {}", self.is_mountpoint);
        match self.free_bytes {
            Some(b) => println!("  free   : {:.1} GB", b as f64 / 1_073_741_824.0),
            None => println!("  free   : unknown"),
        }
    }
}

/// Filesystem types that cannot preserve Unix permissions and are almost always
/// a boot or installer partition. The `ESD-USB` partition on the measured disk is
/// 32 GB of vfat sitting next to the real ntfs3 backup volume — choosing it would
/// produce a backup that cannot be restored with its modes intact.
pub const UNSAFE_FSTYPES: [&str; 3] = ["exfat", "iso9660", "vfat"];

/// Filesystems that do not survive a restart. A removable disk that is NOT mounted
/// leaves its mount point as an empty directory under `/run/media/<user>`, and
/// `udev`/`udisks` mount that parent as `tmpfs`. Proving "not the root filesystem"
/// therefore passes on the parent, so an unmounted external target used to be
/// accepted, printed as `mount: true, fstype: tmpfs`, and the only restore point
/// would have been written to RAM. Refused unconditionally, like the other unsafe
/// types: `--allow-local-backup` means "the internal disk", not "memory".
pub const VOLATILE_FSTYPES: [&str; 3] = ["ramfs", "tmpfs", "devtmpfs"];

/// Prove that `p` really is on a mounted volume before a byte is written.
pub fn prove_target(p: &Path) -> Result<TargetProof> {
    prove_target_with(p, false)
}

/// Same proof, but [`prove_target`]'s mount requirement may be waived by
/// `--allow-local-backup`. The fstype refusal is NEVER waived.
pub fn prove_target_with(p: &Path, allow_local: bool) -> Result<TargetProof> {
    let anchor = nearest_existing(p);
    #[cfg(target_os = "linux")]
    let (is_mountpoint, device, fstype) = {
        let canonical = anchor.canonicalize().unwrap_or_else(|_| anchor.clone());
        match std::fs::read_to_string("/proc/self/mountinfo") {
            Ok(text) => facts_from_entries(&parse_mountinfo(&text), &canonical),
            Err(_) => fallback_mount_facts(&canonical),
        }
    };
    #[cfg(not(target_os = "linux"))]
    let (is_mountpoint, device, fstype) = (true, String::new(), "unknown".to_string());
    finish_proof(p, allow_local, is_mountpoint, device, fstype, &anchor)
}

/// Prove by consulting an explicit mount table.
///
/// Split out so the refusal rules are testable without a real mount and without
/// root — the production path above just supplies the kernel's own table.
#[cfg(test)]
pub fn prove_target_from(
    p: &Path,
    allow_local: bool,
    entries: &[MountEntry],
) -> Result<TargetProof> {
    let anchor = nearest_existing(p);
    let canonical = anchor.canonicalize().unwrap_or_else(|_| anchor.clone());
    let (is_mountpoint, device, fstype) = facts_from_entries(entries, &canonical);
    finish_proof(p, allow_local, is_mountpoint, device, fstype, &anchor)
}

fn finish_proof(
    p: &Path,
    allow_local: bool,
    is_mountpoint: bool,
    device: String,
    fstype: String,
    anchor: &Path,
) -> Result<TargetProof> {
    reject_unsafe_fstype(p, &fstype)?;
    if !is_mountpoint && !allow_local {
        bail!(
            "refusing to use {} as a backup target: it is not a mount point. \
             An unplugged disk must not turn into a directory on the root filesystem. \
             Mount the disk, or pass --allow-local-backup to accept the internal disk.",
            p.display()
        );
    }
    Ok(TargetProof {
        path: p.to_path_buf(),
        device,
        fstype,
        is_mountpoint,
        free_bytes: free_space_bytes(anchor),
    })
}

/// Prove the resolved bulk root, allowing the control root without a proof.
///
/// `bulk == control` is the historical default and must keep working on a plain
/// home directory. Anything else has to pass the mount proof unless the operator
/// explicitly asked for the local disk.
pub fn prove_backup_target(bulk: &Path, control: &Path, allow_local: bool) -> Result<TargetProof> {
    if bulk == control {
        return Ok(TargetProof {
            path: bulk.to_path_buf(),
            device: "local".to_string(),
            fstype: "local".to_string(),
            // The control root is an explicit, documented exception: it is the
            // guard's own directory, not a removable target that can vanish.
            is_mountpoint: true,
            free_bytes: free_space_bytes(&nearest_existing(bulk)),
        });
    }
    if allow_local {
        prove_target_with(bulk, true)
    } else {
        prove_target(bulk)
    }
}

/// Refuse a filesystem type that cannot carry a restore point.
///
/// Pure and injectable on purpose: the test does not need a real vfat mount.
pub fn reject_unsafe_fstype(p: &Path, fstype: &str) -> Result<()> {
    let fstype = fstype.to_ascii_lowercase();
    if VOLATILE_FSTYPES.contains(&fstype.as_str()) {
        bail!(
            "refusing {}: the nearest existing directory is on {} (RAM-backed), which is what an \
             UNMOUNTED removable disk looks like under /run/media/<user>. The restore point would \
             be written to memory and lost on reboot. Mount the volume and retry; --allow-local-backup \
             means the internal disk, not memory.",
            p.display(),
            fstype
        );
    }
    if UNSAFE_FSTYPES.contains(&fstype.as_str()) {
        bail!(
            "refusing {}: {} cannot preserve permissions and is usually a boot or installer partition",
            p.display(),
            fstype
        );
    }
    Ok(())
}

/// Deepest ancestor of `p` that exists (or `p` itself).
///
/// The target directory normally does not exist yet — `/run/media/.../backup` is
/// created by the first run — so the proof is made against the volume that would
/// receive it, not against a path that cannot be stat'ed.
pub fn nearest_existing(p: &Path) -> PathBuf {
    let mut cur = p.to_path_buf();
    loop {
        if cur.exists() {
            return cur;
        }
        match cur.parent() {
            Some(parent) if parent != cur => cur = parent.to_path_buf(),
            _ => return cur,
        }
    }
}

/// `(is_mountpoint, device, fstype)` from an explicit mount table.
///
/// "Not the root filesystem" is the test, not "different mount point": separate
/// btrfs subvolumes such as `/home` share the root device and are therefore not
/// an independent target. A removable disk has its own device.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn facts_from_entries(entries: &[MountEntry], path: &Path) -> (bool, String, String) {
    let Some(entry) = mount_entry_for(entries, path) else {
        return (false, String::new(), "unknown".to_string());
    };
    let root_device = entries
        .iter()
        .find(|e| e.mount_point == Path::new("/"))
        .map(|e| e.device.as_str());
    let on_root = entry.mount_point == Path::new("/")
        || root_device.map(|dev| dev == entry.device).unwrap_or(false);
    (!on_root, entry.device.clone(), entry.fstype.clone())
}

/// Compare st_dev with the parent when mountinfo is unavailable.
#[cfg(target_os = "linux")]
fn fallback_mount_facts(path: &Path) -> (bool, String, String) {
    use std::os::unix::fs::MetadataExt;
    let Ok(md) = std::fs::metadata(path) else {
        return (false, String::new(), "unknown".to_string());
    };
    let is_mountpoint = path
        .parent()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|parent| parent.dev() != md.dev())
        .unwrap_or(false);
    (
        is_mountpoint,
        format!("dev:{}", md.dev()),
        "unknown".to_string(),
    )
}

/// One `/proc/self/mountinfo` row.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct MountEntry {
    pub mount_point: PathBuf,
    pub device: String,
    pub fstype: String,
}

/// Parse mountinfo, honouring the kernel's octal escapes (`\040` for a space).
///
/// Without the unescape, `/run/media/tango/New\040Volume` never matches the path
/// the operator typed, and a perfectly mounted disk reads as "not a mount point".
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_mountinfo(text: &str) -> Vec<MountEntry> {
    text.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 7 {
                return None;
            }
            let sep = parts.iter().position(|f| *f == "-")?;
            if sep + 2 >= parts.len() {
                return None;
            }
            Some(MountEntry {
                mount_point: PathBuf::from(unescape_mountinfo(parts[4])),
                fstype: parts[sep + 1].to_string(),
                device: parts[sep + 2].to_string(),
            })
        })
        .collect()
}

/// The entry whose mount point is the longest prefix of `path` — the volume
/// that actually holds those bytes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn mount_entry_for<'a>(entries: &'a [MountEntry], path: &Path) -> Option<&'a MountEntry> {
    entries
        .iter()
        .filter(|e| path.starts_with(&e.mount_point))
        .max_by_key(|e| e.mount_point.as_os_str().len())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn unescape_mountinfo(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let digits: String = chars.by_ref().take(3).collect();
        match u32::from_str_radix(&digits, 8)
            .ok()
            .and_then(char::from_u32)
        {
            Some(decoded) if digits.len() == 3 => out.push(decoded),
            _ => {
                out.push('\\');
                out.push_str(&digits);
            }
        }
    }
    out
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

/// Command-line fragments that identify a running Desktop app on THIS platform.
///
/// The Windows image is `Hermes.exe`. macOS and Linux ship a bare `Hermes` binary
/// inside a bundle/app directory, and `pgrep -f` matches command LINES, so asking
/// every platform for `Hermes.exe` answered "absent" on a host where the app was
/// running (measured: pid alive, preflight printed `[desktop app] not running`).
/// That turned `--close-desktop` into a no-op and let the packaged rebuild
/// overwrite the files of a running app.
pub fn desktop_app_images() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &["Hermes.exe"]
    }
    #[cfg(target_os = "macos")]
    {
        &["Hermes.app/Contents/MacOS/Hermes", "Hermes Helper"]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        &["linux-unpacked/Hermes", "/Hermes --type=", "hermes-desktop"]
    }
}

/// Combine per-image probe results. A positive match on any name wins, because one
/// confirmed process IS the answer; Unknown only wins when nothing matched and some
/// probe could not run (fail closed, never "absent" from a broken probe).
pub fn combine_proc_states(states: &[ProcState]) -> ProcState {
    let mut unknown: Option<String> = None;
    for s in states {
        match s {
            ProcState::Running => return ProcState::Running,
            ProcState::Unknown(why) => unknown = unknown.or_else(|| Some(why.clone())),
            ProcState::Absent => {}
        }
    }
    match unknown {
        Some(why) => ProcState::Unknown(why),
        None => ProcState::Absent,
    }
}

/// Tri-state state of the Desktop app across every name it can run under here.
pub fn desktop_app_state() -> ProcState {
    let states: Vec<ProcState> = desktop_app_images()
        .iter()
        .map(|i| process_state(i))
        .collect();
    combine_proc_states(&states)
}

/// A `.desktop` entry that claims to launch a Hermes desktop app.
///
/// Two installations can each ship one: the checkout app that `hermes desktop`
/// installs, and a distribution package such as `hermes-desktop`. Both are
/// `Name=Hermes`, so the launcher shows two identical icons and one of them can
/// start a stale build. Preflight reports them instead of guessing which is
/// current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopLauncher {
    pub path: PathBuf,
    /// `user` for `$XDG_DATA_HOME`, `system` for `$XDG_DATA_DIRS`.
    pub scope: &'static str,
    pub name: String,
    pub exec: String,
    /// False when the entry sets `NoDisplay=true` or `Hidden=true`.
    pub visible: bool,
}

/// Application directories in XDG precedence order.
pub fn desktop_launcher_dirs() -> Vec<(PathBuf, &'static str)> {
    let mut dirs = Vec::new();
    let user = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".local/share"))
        });
    if let Some(root) = user {
        dirs.push((root.join("applications"), "user"));
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    for root in data_dirs.split(':').filter(|s| !s.is_empty()) {
        dirs.push((PathBuf::from(root).join("applications"), "system"));
    }
    dirs
}

/// Every launcher entry that claims Hermes, after XDG shadowing.
pub fn desktop_launchers() -> Vec<DesktopLauncher> {
    desktop_launchers_in(&desktop_launcher_dirs())
}

/// Testable core: the first directory wins for a desktop-file ID, exactly as the
/// XDG spec says, so a user override with `Hidden=true` really hides the system
/// entry it shadows.
pub fn desktop_launchers_in(dirs: &[(PathBuf, &'static str)]) -> Vec<DesktopLauncher> {
    let mut seen: Vec<std::ffi::OsString> = Vec::new();
    let mut found = Vec::new();
    for (dir, scope) in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "desktop"))
            .collect();
        paths.sort();
        for path in paths {
            let Some(id) = path.file_name().map(|n| n.to_owned()) else {
                continue;
            };
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some((name, exec, visible)) = parse_desktop_entry(&text) else {
                continue;
            };
            if claims_hermes(&name, &exec) {
                found.push(DesktopLauncher {
                    path,
                    scope,
                    name,
                    exec,
                    visible,
                });
            }
        }
    }
    found
}

/// `(Name, Exec, visible)` from the `[Desktop Entry]` group.
fn parse_desktop_entry(text: &str) -> Option<(String, String, bool)> {
    let mut name = None;
    let mut exec = None;
    let mut kind = None;
    let mut hidden = false;
    let mut in_entry = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_entry = line.eq_ignore_ascii_case("[Desktop Entry]");
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "Name" => name = Some(value.trim().to_string()),
            "Exec" => exec = Some(value.trim().to_string()),
            "Type" => kind = Some(value.trim().to_string()),
            "Hidden" | "NoDisplay" if value.trim().eq_ignore_ascii_case("true") => hidden = true,
            _ => {}
        }
    }
    if kind.is_some_and(|k| !k.eq_ignore_ascii_case("Application")) {
        return None;
    }
    Some((name?, exec?, !hidden))
}

/// Does this entry point at a Hermes desktop app? Match the name and the program
/// basename, never an arbitrary substring: `Hermes Guard Dashboard` must not be
/// counted as a second Hermes Desktop.
fn claims_hermes(name: &str, exec: &str) -> bool {
    let name = name.to_lowercase();
    let exec = exec.to_lowercase();
    let program = exec
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c| c == '"' || c == '\'');
    let base = program.rsplit('/').next().unwrap_or(program);
    let by_name =
        name == "hermes" || name.starts_with("hermes desktop") || name.starts_with("hermes agent");
    let by_exec = base == "hermes" || base == "hermes-desktop" || exec.contains("/hermes desktop");
    by_name || by_exec
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

/// Close the Desktop app under every name it can run as on this platform.
///
/// `--close-desktop` used to pass the Windows image name on every host, so on
/// macOS and Linux it closed nothing and the update proceeded with the app alive.
/// Reports the same shape as [`close_process`]; the detail names which image was
/// actually closed, so a report is never a bare "closed".
pub fn close_desktop_app(
    graceful: std::time::Duration,
    forced: std::time::Duration,
) -> Result<CloseReport> {
    let mut details: Vec<String> = Vec::new();
    let mut survivors: Vec<u32> = Vec::new();
    let mut closed_all = true;
    let mut handled = 0usize;

    for image in desktop_app_images() {
        match process_state(image) {
            ProcState::Absent => {}
            ProcState::Unknown(why) => {
                bail!("cannot verify whether {image} is running ({why}) - treating it as running")
            }
            ProcState::Running => {
                let report = close_process(image, graceful, forced)?;
                handled += 1;
                if !report.closed {
                    closed_all = false;
                    survivors.extend(report.survivors.iter().copied());
                }
                details.push(format!("{image}: {}", report.detail));
            }
        }
    }

    survivors.sort_unstable();
    survivors.dedup();
    Ok(CloseReport {
        closed: closed_all,
        detail: if handled == 0 {
            "no desktop app process was running".to_string()
        } else {
            details.join("; ")
        },
        survivors,
    })
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

        let none = std::collections::BTreeSet::new();
        let bulk = home.join("backups");
        assert_eq!(
            profiles_gone_since_backup(&home, &none, &bulk),
            vec![
                "ancient (last seen 20250101-000000)".to_string(),
                "elevate (last seen 20260101-000000)".to_string(),
            ],
            "every backup is scanned, and the newest sighting wins"
        );

        // a RETIRED profile is an operator decision, not an anomaly
        let retired: std::collections::BTreeSet<String> =
            ["elevate".to_string()].into_iter().collect();
        assert_eq!(
            profiles_gone_since_backup(&home, &retired, &bulk),
            vec!["ancient (last seen 20250101-000000)".to_string()]
        );

        // no backups at all -> nothing claimed
        let bare = std::env::temp_dir().join(format!("hug-bare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&bare);
        std::fs::create_dir_all(&bare).unwrap();
        assert!(profiles_gone_since_backup(&bare, &none, &bare.join("backups")).is_empty());

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

    /// Serialise the one test that touches a process-global env var.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Precedence is the whole contract: an explicit flag must beat the env var,
    /// and the env var must beat the default. The env path is NOT validated as a
    /// mount here — the proof belongs to the command that writes.
    #[test]
    fn bulk_root_precedence_flag_then_env_then_control() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = PathBuf::from("/tmp/hug-home");
        let saved = std::env::var_os("HERMES_GUARD_BULK_ROOT");
        std::env::remove_var("HERMES_GUARD_BULK_ROOT");

        assert_eq!(bulk_root(&home, None).unwrap(), home.join("backups"));
        std::env::set_var("HERMES_GUARD_BULK_ROOT", "/mnt/not-a-real-mount/env");
        assert_eq!(
            bulk_root(&home, None).unwrap(),
            PathBuf::from("/mnt/not-a-real-mount/env"),
            "an env path is returned even though it cannot be a mount"
        );
        assert_eq!(
            bulk_root(&home, Some(Path::new("/mnt/flag/backup"))).unwrap(),
            PathBuf::from("/mnt/flag/backup"),
            "the explicit flag beats the env var"
        );

        match saved {
            Some(v) => std::env::set_var("HERMES_GUARD_BULK_ROOT", v),
            None => std::env::remove_var("HERMES_GUARD_BULK_ROOT"),
        }
    }

    fn mount(mount_point: &str, device: &str, fstype: &str) -> MountEntry {
        MountEntry {
            mount_point: PathBuf::from(mount_point),
            device: device.to_string(),
            fstype: fstype.to_string(),
        }
    }

    /// The Desktop app runs as a bare `Hermes` binary on macOS and Linux. A probe
    /// that only knows the Windows image name answered "absent" on a host where
    /// the app was running, which made `--close-desktop` a no-op.
    #[test]
    fn desktop_probe_knows_every_platform_name() {
        let images = desktop_app_images();
        assert!(!images.is_empty(), "a probe with no names can never match");
        #[cfg(windows)]
        assert!(images.contains(&"Hermes.exe"));
        #[cfg(target_os = "macos")]
        assert!(images.iter().any(|i| i.contains("MacOS/Hermes")));
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            assert!(
                images.iter().any(|i| i.starts_with("linux-unpacked/")),
                "the packaged Linux app must be probeable: {images:?}"
            );
            assert!(
                !images.contains(&"Hermes.exe"),
                "a Windows image name can never match on Linux"
            );
        }
    }

    /// One confirmed process IS the answer; Unknown only wins when nothing matched
    /// and a probe could not run. Both directions, so the combiner cannot silently
    /// pass a running app or invent one.
    #[test]
    fn probe_combiner_prefers_running_and_fails_closed() {
        assert_eq!(
            combine_proc_states(&[ProcState::Absent, ProcState::Running]),
            ProcState::Running
        );
        assert_eq!(
            combine_proc_states(&[ProcState::Absent, ProcState::Absent]),
            ProcState::Absent
        );
        assert!(matches!(
            combine_proc_states(&[
                ProcState::Absent,
                ProcState::Unknown("pgrep missing".into())
            ]),
            ProcState::Unknown(_)
        ));
        assert_eq!(
            combine_proc_states(&[ProcState::Unknown("x".into()), ProcState::Running]),
            ProcState::Running,
            "a positive hit outranks an unrelated probe failure"
        );
    }

    /// The bug this closes: an UNMOUNTED removable disk leaves its mount point on a
    /// tmpfs under /run/media/<user>, the "not the root filesystem" test passes on
    /// that parent, and the only restore point would be written to RAM.
    #[test]
    fn refuses_a_target_that_lands_on_tmpfs() {
        let dir = std::env::temp_dir().join(format!("hug-tmpfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let parent = std::fs::canonicalize(&dir)
            .unwrap()
            .to_string_lossy()
            .to_string();
        // udisks layout: the parent exists as a tmpfs, the volume's own directory does not.
        let entries = vec![
            mount("/", "/dev/root", "ext4"),
            mount(&parent, "tmpfs", "tmpfs"),
        ];
        let target = dir.join("New Volume/mcpvot-backup/hermes-guard");
        let refused = prove_target_from(&target, false, &entries);
        assert!(
            refused.is_err(),
            "an unmounted volume under a tmpfs parent must be refused"
        );
        let msg = format!("{:#}", refused.unwrap_err());
        assert!(
            msg.contains("RAM-backed") && msg.contains("Mount the volume"),
            "the refusal must say why and what to do: {msg}"
        );
        assert!(!target.exists(), "the proof must never create the target");

        // --allow-local-backup does NOT waive a volatile filesystem: it means the
        // internal disk, never memory.
        assert!(prove_target_from(&target, true, &entries).is_err());

        // The same path is accepted once the volume is really mounted. A real
        // mount makes the volume's own directory exist, which is exactly what the
        // proof anchors on, so the fixture has to create it too. The mount entry
        // must use the CANONICAL path: the proof canonicalises its anchor, and on
        // macOS (/var -> /private/var) and Windows (8.3 names, \\?\ prefix) the
        // raw temp path never matches the table.
        let volume = std::fs::canonicalize(&dir).unwrap().join("New Volume");
        std::fs::create_dir_all(&volume).unwrap();
        let mounted = vec![
            mount("/", "/dev/root", "ext4"),
            mount(&parent, "tmpfs", "tmpfs"),
            mount(volume.to_string_lossy().as_ref(), "/dev/sda2", "ntfs3"),
        ];
        let proof = prove_target_from(&target, false, &mounted).unwrap();
        assert_eq!(proof.fstype, "ntfs3");
        assert_eq!(proof.device, "/dev/sda2");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The boot/installer refusal keeps its own reason, and a real backup
    /// filesystem is never refused.
    #[test]
    fn fstype_refusals_name_their_reason() {
        let p = Path::new("/run/media/u/ESD-USB");
        let vfat = format!("{:#}", reject_unsafe_fstype(p, "vfat").unwrap_err());
        assert!(vfat.contains("cannot preserve permissions"), "{vfat}");
        let tmpfs = format!("{:#}", reject_unsafe_fstype(p, "tmpfs").unwrap_err());
        assert!(tmpfs.contains("RAM-backed"), "{tmpfs}");
        for ok in ["ntfs3", "ext4", "btrfs", "xfs", "apfs"] {
            assert!(
                reject_unsafe_fstype(p, ok).is_ok(),
                "{ok} is a real backup filesystem"
            );
        }
    }

    /// A plain directory on the root filesystem is refused, and a path that does
    /// not exist yet is refused too — and neither refusal creates anything.
    #[test]
    fn prove_target_refuses_the_root_filesystem_and_creates_nothing() {
        let dir = std::env::temp_dir().join(format!("hug-proof-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let root_only = vec![mount("/", "/dev/root", "ext4")];
        let existing = prove_target_from(&dir, false, &root_only);
        assert!(existing.is_err());
        let msg = format!("{:#}", existing.unwrap_err());
        assert!(
            msg.contains("it is not a mount point") && msg.contains("--allow-local-backup"),
            "the refusal must name the rule and the way out: {msg}"
        );

        let deep = dir.join("new/backup/dir");
        let missing = prove_target_from(&deep, false, &root_only);
        assert!(missing.is_err(), "a path that must be created is refused");
        assert!(!deep.exists(), "the proof must not create the target");

        // The operator can still choose the local disk deliberately.
        let allowed = prove_target_from(&deep, true, &root_only).unwrap();
        assert!(!allowed.is_mountpoint);
        assert!(
            !deep.exists(),
            "--allow-local-backup must not create it either"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A target under a mounted non-root volume is accepted, and the proof carries
    /// that volume's device and fstype.
    #[test]
    fn prove_target_accepts_a_mounted_volume() {
        let dir = std::env::temp_dir().join(format!("hug-proof-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Canonicalise the same way the proof does: /var/folders on macOS and
        // 8.3-expanded paths on Windows would otherwise never match the mount
        // table entry and the test would fail for the wrong reason.
        let parent = std::fs::canonicalize(&dir)
            .unwrap()
            .to_string_lossy()
            .to_string();
        let entries = vec![
            mount("/", "/dev/root", "ext4"),
            mount(&parent, "/dev/sda2", "ntfs3"),
        ];
        let target = dir.join("mcpvot-backup/hermes-guard");
        let proof = prove_target_from(&target, false, &entries).unwrap();
        assert_eq!(proof.device, "/dev/sda2");
        assert_eq!(proof.fstype, "ntfs3");
        assert!(proof.is_mountpoint);
        assert!(!target.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The control root is legal without any mount proof (backwards compatibility).
    #[test]
    fn control_root_is_accepted_without_a_proof() {
        let home = std::env::temp_dir().join(format!("hug-control-{}", std::process::id()));
        let control = control_root(&home);
        let proof = prove_backup_target(&control, &control, false).unwrap();
        assert!(proof.is_mountpoint);
        assert_eq!(proof.device, "local");
        assert!(
            !control.exists(),
            "proving must not create the control root"
        );
    }

    /// Installer/boot filesystems cannot preserve permission bits, so they are
    /// refused by TYPE, independently of whether the path is a mount point.
    #[test]
    fn prove_target_rejects_boot_and_installer_filesystems() {
        for fs in ["vfat", "VFAT", "exfat", "iso9660"] {
            assert!(
                reject_unsafe_fstype(Path::new("/mnt/x"), fs).is_err(),
                "{fs} must be refused"
            );
        }
        for fs in ["ntfs3", "ext4", "btrfs", "apfs", "unknown"] {
            assert!(reject_unsafe_fstype(Path::new("/mnt/x"), fs).is_ok());
        }
    }

    /// mountinfo escapes spaces as `\040`; without decoding it, the real SanDisk
    /// mount at `/run/media/tango/New Volume` can never match what was typed.
    #[test]
    fn mountinfo_parses_escaped_mount_points_and_longest_prefix() {
        let text = "\
413 29 8:2 / /run/media/tango/New\\040Volume rw,relatime shared:468 - ntfs3 /dev/sda2 rw\n\
350 29 8:1 / /run/media/tango/ESD-USB rw,relatime shared:453 - vfat /dev/sda1 rw\n\
29 33 0:26 / /run rw,nosuid shared:12 - tmpfs run rw\n";
        let entries = parse_mountinfo(text);
        assert_eq!(
            entries[0].mount_point,
            PathBuf::from("/run/media/tango/New Volume")
        );
        assert_eq!(entries[0].device, "/dev/sda2");
        assert_eq!(entries[0].fstype, "ntfs3");
        assert_eq!(entries[1].fstype, "vfat");

        let target = Path::new("/run/media/tango/New Volume/mcpvot-backup/hermes-guard");
        let hit = mount_entry_for(&entries, target).unwrap();
        assert_eq!(hit.device, "/dev/sda2");

        let (is_mount, device, fstype) = facts_from_entries(&entries, target);
        assert!(is_mount);
        assert_eq!(device, "/dev/sda2");
        assert_eq!(fstype, "ntfs3");
    }

    fn write_desktop(dir: &Path, id: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(id), body).unwrap();
    }

    fn launcher_fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("hug-launchers-{tag}-{}", std::process::id()));
        let user = base.join("user");
        let system = base.join("system");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&system).unwrap();
        (base, user, system)
    }

    /// Measured 2026-11-06: the checkout app and the `hermes-desktop` package
    /// both installed `Name=Hermes`, and the launcher showed two identical icons.
    #[test]
    fn desktop_launchers_lists_both_installs() {
        let (base, user, system) = launcher_fixture("both");
        write_desktop(
            &user,
            "com.nousresearch.hermes.desktop",
            "[Desktop Entry]\nType=Application\nName=Hermes\nExec=/home/x/.local/bin/hermes desktop --skip-build\n",
        );
        write_desktop(
            &system,
            "hermes-desktop.desktop",
            "[Desktop Entry]\nType=Application\nName=Hermes\nExec=hermes-desktop %U\n",
        );
        let got = desktop_launchers_in(&[(user.clone(), "user"), (system.clone(), "system")]);
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|l| l.visible));
        assert_eq!(got.iter().filter(|l| l.scope == "user").count(), 1);
        assert_eq!(got.iter().filter(|l| l.scope == "system").count(), 1);
        std::fs::remove_dir_all(&base).ok();
    }

    /// A user-level entry shadows the system entry with the same desktop-file
    /// ID; if it is hidden, the menu is down to one icon even though the
    /// packaged file still exists on disk.
    #[test]
    fn desktop_launchers_honour_xdg_shadowing_and_hidden() {
        let (base, user, system) = launcher_fixture("shadow");
        let hidden = "[Desktop Entry]\nType=Application\nName=Hermes Desktop (packaged)\nExec=hermes-desktop %U\nNoDisplay=true\nHidden=true\n";
        write_desktop(&user, "hermes-desktop.desktop", hidden);
        write_desktop(&system, "hermes-desktop.desktop", hidden);
        write_desktop(
            &user,
            "com.nousresearch.hermes.desktop",
            "[Desktop Entry]\nType=Application\nName=Hermes\nExec=/home/x/.local/bin/hermes desktop --skip-build\n",
        );
        let got = desktop_launchers_in(&[(user.clone(), "user"), (system, "system")]);
        assert_eq!(
            got.len(),
            2,
            "the shadowed system entry must not be listed twice"
        );
        assert_eq!(got.iter().filter(|l| l.visible).count(), 1);
        assert!(got.iter().any(|l| !l.visible && l.path.starts_with(&user)));
        std::fs::remove_dir_all(&base).ok();
    }

    /// The guard dashboard is not a Hermes Desktop install. Match the exact
    /// name or the program basename, never a loose "hermes" substring.
    #[test]
    fn desktop_launchers_ignore_guard_and_unrelated_entries() {
        let (base, user, system) = launcher_fixture("ignore");
        write_desktop(
            &user,
            "guard.desktop",
            "[Desktop Entry]\nType=Application\nName=Hermes Guard Dashboard\nExec=hermes-guard-dashboard\n",
        );
        write_desktop(
            &user,
            "files.desktop",
            "[Desktop Entry]\nType=Application\nName=Files\nExec=nautilus\n",
        );
        write_desktop(
            &user,
            "com.nousresearch.hermes.desktop",
            "[Desktop Entry]\nType=Application\nName=Hermes\nExec=hermes desktop\n",
        );
        let got = desktop_launchers_in(&[(user, "user"), (system, "system")]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "Hermes");
        std::fs::remove_dir_all(&base).ok();
    }
}
