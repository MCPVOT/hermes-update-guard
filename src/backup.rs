use anyhow::{bail, Context, Result};
use chrono::Local;
use rusqlite::OpenFlags;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::env;

/// Small per-profile files copied as-is. `.env` and `auth.json` are skipped with
/// `--no-secrets`; the rest are ordinary state.
const SMALL_FILES: [&str; 5] = [
    "config.yaml",
    ".env",
    "auth.json",
    "gateway_state.json",
    "channel_directory.json",
];

/// One thing the run did NOT copy, and why. The manifest carries these so
/// `verify` can call a restore point PARTIAL instead of implying it is whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skip {
    pub path: String,
    pub reason: String,
}

/// `pre-update-<stamp>/manifest.json` — the record of what this restore point
/// actually contains, and on which volume it was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub guard_version: String,
    pub created: String,
    pub secrets_included: bool,
    pub profiles: Vec<String>,
    pub files: usize,
    pub bytes: u64,
    pub target: env::TargetProof,
    pub skipped: Vec<Skip>,
}

// ---------------------------------------------------------------------------
// public entry points
// ---------------------------------------------------------------------------

/// WAL-safe backup of every profile: small files, content dirs (memories/skills/
/// scripts/cron), and a STANDALONE state.db snapshot via the SQLite online-backup
/// API (WAL folded in).
///
/// Port of update-hermes-v2.ps1 phase 3 (H1-H5): online-backup API, integrity_check +
/// message count per db, COMPLETENESS GATE refusing to continue when a profile's db
/// is missing.
pub fn run(home: &Path, dry_run: bool, cfg: &Config) -> Result<PathBuf> {
    run_with(home, dry_run, cfg, &mut |_, _| {})
}

/// Same as [`run`], reporting each profile's database result as it completes so a
/// live viewer can show per-profile progress during the long, silent copy phase.
pub fn run_with(
    home: &Path,
    dry_run: bool,
    cfg: &Config,
    on_db: &mut dyn FnMut(&str, bool),
) -> Result<PathBuf> {
    let stamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let backup_root = cfg.bulk_root.clone();
    let dir = backup_root.join(format!("pre-update-{}", stamp));

    println!("== PRE-UPDATE BACKUP ({}) ==", dir.display());

    // ---- mount proof ------------------------------------------------------
    // Before anything is created: an unplugged disk must not silently become a
    // directory on the internal SSD. `preflight` proves read-only; this is the
    // path that would actually write.
    let proof = env::prove_backup_target(&backup_root, &cfg.control_root, cfg.allow_local_backup)?;
    proof.print();
    if !proof.is_mountpoint {
        println!("  [warn] --allow-local-backup: writing to the internal disk on purpose");
    }
    println!(
        "  content: {}",
        if cfg.content.is_empty() {
            "(none)".to_string()
        } else {
            cfg.content.join(", ")
        }
    );
    println!(
        "  secrets: {}",
        if cfg.no_secrets {
            "EXCLUDED (.env, auth.json) - --no-secrets"
        } else {
            "included (.env, auth.json)"
        }
    );

    // ---- disk-space gate -------------------------------------------------
    // The fleet's databases are large (one measured at 2.0 GB, the whole fleet
    // at ~2.9 GB). A backup needs that much again, plus room for the WAL that
    // lands while it runs. Refuse BEFORE copying rather than fill the volume.
    let mut needed: u64 = 0;
    for p in env::profiles(home).iter().map(|s| s.as_str()) {
        let db = env::state_db(home, p);
        if db.exists() {
            needed += env::dir_size_bytes(&db);
        }
    }
    let needed_with_headroom = needed + needed / 2 + 512 * 1024 * 1024; // 1.5x + 512MB
    let free = proof
        .free_bytes
        .or_else(|| env::free_space_bytes(&env::nearest_existing(&backup_root)));
    match free {
        Some(free) if free < needed_with_headroom => {
            println!(
                "  [FAIL] not enough disk for the backup: need ~{:.1} GB, {} GB free",
                needed_with_headroom as f64 / 1_073_741_856.0,
                free / 1_073_741_856
            );
            bail!("insufficient disk space for a verified backup");
        }
        Some(free) => println!(
            "  disk: {} GB free, backup needs ~{:.1} GB",
            free / 1_073_741_856,
            needed_with_headroom as f64 / 1_073_741_856.0
        ),
        None => println!(
            "  disk: free space unknown - proceeding (cannot prove the volume can take it)"
        ),
    }

    if dry_run {
        println!("[dry-run] plan only - nothing written");
        let mut expected = 0;
        for p in env::profiles(home).iter().map(|s| s.as_str()) {
            if env::state_db(home, p).exists() {
                expected += 1;
                println!(
                    "  [plan] {} state.db -> {}",
                    p,
                    dir.join(format!("{}/state.db", p)).display()
                );
            }
        }
        println!(
            "[dry-run] would back up {} profile databases + content dirs",
            expected
        );
        return Ok(dir);
    }

    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let mut total = 0usize;
    let mut bytes = 0u64;
    let mut skipped: Vec<Skip> = Vec::new();
    let profiles = env::profiles(home);

    // 3a. small files + content dirs per profile
    for p in profiles.iter().map(|s| s.as_str()) {
        let src = env::profile_dir(home, p);
        if !src.is_dir() {
            continue;
        }
        let dst = dir.join(p);
        fs::create_dir_all(&dst)?;
        for f in SMALL_FILES {
            let s = src.join(f);
            if !s.is_file() {
                continue;
            }
            if cfg.no_secrets && (f == ".env" || f == "auth.json") {
                skipped.push(Skip {
                    path: format!("{p}/{f}"),
                    reason: "secret".to_string(),
                });
                continue;
            }
            bytes += fs::copy(&s, dst.join(f))?;
            total += 1;
            println!("  {} {}", p, f);
        }
        for sub in crate::config::CONTENT_KINDS {
            if !cfg.wants_content(sub) {
                continue;
            }
            let s = src.join(sub);
            if s.is_dir() {
                let rel = PathBuf::from(p).join(sub);
                copy_dir(
                    &s,
                    &dst.join(sub),
                    &rel,
                    &mut skipped,
                    &mut total,
                    &mut bytes,
                )?;
                println!("  {} {}/ (dir)", p, sub);
            }
        }
    }

    // 3b. databases via SQLite online backup -> standalone, verified
    let mut fails = 0usize;
    for p in profiles.iter().map(|s| s.as_str()) {
        let src_db = env::state_db(home, p);
        if !src_db.exists() {
            continue;
        }
        // Hermes' OWN pre-update snapshot skips any state.db over 1 GB
        // ("Quick snapshot skipped oversized DB file(s)"). Say so: this guard's
        // copy is then the only restore point that has the file.
        let size = crate::context_health::db_size(home, p);
        if size > 1_073_741_824 {
            println!(
                "  note {} state.db is {:.1} MB - Hermes' built-in snapshot SKIPS files over 1 GB; this backup is the only copy",
                p,
                size as f64 / 1_048_576.0
            );
        }
        let dst_db = dir.join(p).join("state.db");
        fs::create_dir_all(dst_db.parent().unwrap())?;
        match backup_db(&src_db, &dst_db) {
            Ok((ok, msg)) => {
                if !ok {
                    fails += 1;
                }
                println!("  {} {} state.db", if ok { "ok  " } else { "FAIL" }, p);
                println!("      {}", msg);
                on_db(p, ok);
                if let Ok(md) = fs::metadata(&dst_db) {
                    bytes += md.len();
                    total += 1;
                }
            }
            Err(e) => {
                fails += 1;
                println!("  FAIL {} state.db backup failed: {:#}", p, e);
                on_db(p, false);
            }
        }
    }

    // 3c. COMPLETENESS GATE
    let mut missing: Vec<String> = Vec::new();
    for p in profiles.iter().map(|s| s.as_str()) {
        if env::state_db(home, p).exists() && !dir.join(p).join("state.db").exists() {
            missing.push(p.to_string());
        }
    }
    if !missing.is_empty() {
        bail!(
            "backup INCOMPLETE - no verified state.db for: {} - refusing to treat this backup as whole",
            missing.join(", ")
        );
    }

    // 3d. git state we must never lose
    let repo = home.join("hermes-agent");
    if repo.join(".git").exists() {
        env::run_capture(
            Path::new("git"),
            &["-C", repo.to_string_lossy().as_ref(), "rev-parse", "HEAD"],
            None,
        )
        .ok()
        .map(|(_, h)| fs::write(dir.join("HEAD-before.txt"), h.trim()));
        env::run_capture(
            Path::new("git"),
            &["-C", repo.to_string_lossy().as_ref(), "diff"],
            None,
        )
        .ok()
        .map(|(_, d)| fs::write(dir.join("working-tree.patch"), d));
        env::run_capture(
            Path::new("git"),
            &["-C", repo.to_string_lossy().as_ref(), "stash", "list"],
            None,
        )
        .ok()
        .map(|(_, s)| fs::write(dir.join("stash-list.txt"), s));
        env::run_capture(
            Path::new("git"),
            &[
                "-C",
                repo.to_string_lossy().as_ref(),
                "stash",
                "show",
                "-p",
                "stash@{0}",
            ],
            None,
        )
        .ok()
        .map(|(_, sp)| fs::write(dir.join("stash-0.patch"), sp));
    }
    let (_, version) = env::run_capture(&env::venv_hermes(home), &["--version"], None)
        .unwrap_or((-1, String::new()));
    fs::write(dir.join("version-before.txt"), version)?;

    if fails > 0 {
        bail!(
            "database backup FAILED for {} profile(s) - DO NOT update from an unverified backup",
            fails
        );
    }

    // ---- manifest ---------------------------------------------------------
    // Written only after the completeness gate, so a manifest never describes a
    // run that already refused to be treated as whole.
    let manifest = Manifest {
        guard_version: env!("CARGO_PKG_VERSION").to_string(),
        created: Local::now().to_rfc3339(),
        secrets_included: !cfg.no_secrets,
        profiles: profiles.clone(),
        files: total,
        bytes,
        target: proof.clone(),
        skipped,
    };
    write_manifest(&dir, &manifest)?;

    println!(
        "backup complete and verified: {} files + {} dbs, {:.1} MB",
        total,
        db_count(home),
        bytes as f64 / 1_048_576.0
    );
    println!("backup dir: {}", dir.display());
    if !manifest.skipped.is_empty() {
        println!(
            "  skipped {} entr(ies) - see manifest.json",
            manifest.skipped.len()
        );
    }

    // ---- retention --------------------------------------------------------
    prune(
        &backup_root,
        cfg.keep,
        &dir,
        proof.is_mountpoint || cfg.bulk_is_control(),
    );
    Ok(dir)
}

/// Persist the manifest next to the payload it describes.
pub fn write_manifest(dir: &Path, m: &Manifest) -> Result<()> {
    let path = dir.join("manifest.json");
    let text = serde_json::to_string_pretty(m)?;
    fs::write(&path, text).with_context(|| format!("write {}", path.display()))
}

/// `None` when there is no manifest (a pre-0.4.0 run) or it cannot be parsed.
pub fn read_manifest(dir: &Path) -> Option<Manifest> {
    let text = fs::read_to_string(dir.join("manifest.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Newest `pre-update-*` directory on the bulk root.
pub fn newest_run(bulk_root: &Path) -> Option<PathBuf> {
    list_runs(bulk_root).pop()
}

fn list_runs(bulk_root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(bulk_root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("pre-update-"))
                .unwrap_or(false)
        })
        .collect();
    dirs.sort();
    dirs
}

/// Keep the newest `keep` runs on the bulk root, never the one just written.
///
/// Called only after a verified backup. `keep == 0` is today's behaviour: keep
/// everything. A target that failed its mount proof is never pruned — deleting
/// directories on an unproven path is how a wrong path becomes data loss.
fn prune(bulk_root: &Path, keep: usize, current: &Path, mount_ok: bool) {
    if keep == 0 {
        return;
    }
    if !mount_ok {
        println!(
            "  prune : skipped - {} did not pass its mount proof; deleting nothing there",
            bulk_root.display()
        );
        return;
    }
    let dirs = list_runs(bulk_root);
    if dirs.len() <= keep {
        println!(
            "  prune : {} run(s) on the target, --keep {} - nothing to remove",
            dirs.len(),
            keep
        );
        return;
    }
    let remove = dirs.len() - keep;
    for old in dirs.iter().take(remove) {
        if old == current {
            println!("  prune : keeping the run just written");
            continue;
        }
        // Defensive: only ever delete a direct child named like a run.
        let safe = old.parent() == Some(bulk_root)
            && old
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("pre-update-"))
                .unwrap_or(false);
        if !safe {
            continue;
        }
        match fs::remove_dir_all(old) {
            Ok(()) => println!(
                "  prune : removed {}",
                old.file_name().unwrap_or_default().to_string_lossy()
            ),
            Err(e) => println!("  prune : could not remove {}: {}", old.display(), e),
        }
    }
}

fn db_count(home: &Path) -> usize {
    env::profiles(home)
        .iter()
        .filter(|p| env::state_db(home, p).exists())
        .count()
}

/// SQLite online-backup: source opened read-only via URI (WAL-safe with live writers),
/// destination a standalone db. Returns (ok, detail-string) with integrity + message count.
fn backup_db(src: &Path, dst: &Path) -> Result<(bool, String)> {
    let uri = format!(
        "file:{}?mode=ro",
        src.to_str().unwrap_or("").replace('\\', "/")
    );
    let src_conn = rusqlite::Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("open source {}", src.display()))?;

    src_conn
        .backup(
            rusqlite::DatabaseName::Main,
            dst,
            None::<fn(rusqlite::backup::Progress)>,
        )
        .with_context(|| "online backup failed")?;
    drop(src_conn);

    // re-open AFTER the backup wrote the file (the backup() helper opens dst itself)
    let dst_conn = rusqlite::Connection::open(dst)
        .with_context(|| format!("re-open dest {}", dst.display()))?;
    let ok: String = dst_conn.query_row("pragma integrity_check", [], |r| r.get(0))?;
    let msgs: i64 = dst_conn
        .query_row("select count(*) from messages", [], |r| r.get(0))
        .unwrap_or(-1);
    let mb = fs::metadata(dst)
        .map(|m| m.len() as f64 / 1048576.0)
        .unwrap_or(0.0);
    Ok((
        ok == "ok",
        format!("integrity={} messages={} {:.1}MB", ok, msgs, mb),
    ))
}

/// Recursively copy `src` into `dst`, recording everything deliberately skipped.
///
/// Skips:
///   - file names starting with `.` (`dot-cache`) — the measured fleet carries an
///     85 MB regenerable `skills/.hub` cache that is not a restore point;
///   - symlinks (`symlink`) — the only ones present point into /usr/share/omarchy
///     and are regenerable system content. `DirEntry::file_type` does not follow
///     them, so a directory symlink is never descended.
fn copy_dir(
    src: &Path,
    dst: &Path,
    rel: &Path,
    skipped: &mut Vec<Skip>,
    files: &mut usize,
    bytes: &mut u64,
) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let s = entry.path();
        let name = entry.file_name();
        let d = dst.join(&name);
        let child_rel = rel.join(&name);

        if name.to_string_lossy().starts_with('.') {
            skipped.push(Skip {
                path: manifest_path(&child_rel),
                reason: "dot-cache".to_string(),
            });
            continue;
        }

        let ft = entry.file_type()?;
        if ft.is_symlink() {
            skipped.push(Skip {
                path: manifest_path(&child_rel),
                reason: "symlink".to_string(),
            });
            continue;
        }
        if ft.is_dir() {
            copy_dir(&s, &d, &child_rel, skipped, files, bytes)?;
        } else {
            *bytes += fs::copy(&s, &d)?;
            *files += 1;
        }
    }
    Ok(())
}

/// Manifest paths are relative to the run directory and always use `/`.
fn manifest_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigInput;

    fn cfg(home: &Path) -> Config {
        let control = env::control_root(home);
        Config {
            control_root: control.clone(),
            bulk_root: control,
            no_secrets: false,
            content: crate::config::parse_content(None).unwrap(),
            keep: 0,
            allow_local_backup: false,
            allow_lineage_change: false,
        }
    }

    fn home(tag: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("hug-backup-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&h);
        fs::create_dir_all(h.join("profiles/alpha/memories")).unwrap();
        fs::write(h.join("config.yaml"), "model: {}\n").unwrap();
        fs::write(h.join("profiles/alpha/config.yaml"), "model: {}\n").unwrap();
        h
    }

    /// The copy rules are only trustworthy if the skip list is complete: a file
    /// that is not copied and not recorded is a restore point that lies.
    #[test]
    fn copy_dir_skips_dot_entries_and_symlinks_and_records_them() {
        let root = std::env::temp_dir().join(format!("hug-copydir-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let src = root.join("skills");
        let dst = root.join("out");
        fs::create_dir_all(src.join(".hub/cache")).unwrap();
        fs::create_dir_all(src.join("real")).unwrap();
        fs::write(src.join("SKILL.md"), b"hello").unwrap();
        fs::write(src.join("real/notes.txt"), b"notes").unwrap();
        fs::write(src.join(".hub/cache/blob"), vec![0u8; 4096]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/usr/share/omarchy", src.join("omarchy")).unwrap();

        let mut skipped = Vec::new();
        let mut files = 0usize;
        let mut bytes = 0u64;
        copy_dir(
            &src,
            &dst,
            Path::new("alpha/skills"),
            &mut skipped,
            &mut files,
            &mut bytes,
        )
        .unwrap();

        assert!(dst.join("SKILL.md").is_file());
        assert!(dst.join("real/notes.txt").is_file());
        assert!(!dst.join(".hub").exists(), "dot-directories are not copied");
        #[cfg(unix)]
        assert!(!dst.join("omarchy").exists(), "symlinks are not copied");
        assert_eq!(files, 2);
        assert_eq!(bytes, 10);

        let reasons: Vec<(&str, &str)> = skipped
            .iter()
            .map(|s| (s.path.as_str(), s.reason.as_str()))
            .collect();
        assert!(reasons.contains(&("alpha/skills/.hub", "dot-cache")));
        #[cfg(unix)]
        assert!(reasons.contains(&("alpha/skills/omarchy", "symlink")));

        let _ = fs::remove_dir_all(&root);
    }

    /// `--no-secrets` must leave the two credential files out AND say so in the
    /// manifest. Copying them silently would make the manifest a false record.
    #[test]
    fn no_secrets_leaves_env_and_auth_out_and_records_it() {
        let h = home("secrets");
        fs::write(h.join(".env"), "API_KEY=super-secret\n").unwrap();
        fs::write(h.join("auth.json"), "{\"token\":\"super-secret\"}").unwrap();
        fs::write(h.join("gateway_state.json"), "{}").unwrap();

        let mut c = cfg(&h);
        c.no_secrets = true;
        let dir = run(&h, false, &c).unwrap();

        assert!(!dir.join("default/.env").exists());
        assert!(!dir.join("default/auth.json").exists());
        assert!(dir.join("default/config.yaml").exists());
        assert!(dir.join("default/gateway_state.json").exists());

        let m = read_manifest(&dir).expect("manifest");
        assert!(!m.secrets_included);
        assert!(m
            .skipped
            .iter()
            .any(|s| s.path == "default/.env" && s.reason == "secret"));
        assert!(m
            .skipped
            .iter()
            .any(|s| s.path == "default/auth.json" && s.reason == "secret"));

        let _ = fs::remove_dir_all(&h);
    }

    /// Default behaviour is unchanged: with no flags the secrets are present and
    /// the manifest says so.
    #[test]
    fn default_run_still_includes_secrets() {
        let h = home("with-secrets");
        fs::write(h.join(".env"), "API_KEY=x\n").unwrap();
        fs::write(h.join("auth.json"), "{}\n").unwrap();
        let dir = run(&h, false, &cfg(&h)).unwrap();
        assert!(dir.join("default/.env").is_file());
        let m = read_manifest(&dir).unwrap();
        assert!(m.secrets_included);
        assert!(m.skipped.is_empty());
        let _ = fs::remove_dir_all(&h);
    }

    /// The manifest is the contract `verify` reads; it has to survive serde in
    /// both directions with the target proof intact.
    #[test]
    fn manifest_round_trips_through_serde() {
        let m = Manifest {
            guard_version: "0.4.0".into(),
            created: "2026-11-04T18:00:00+00:00".into(),
            secrets_included: false,
            profiles: vec!["default".into(), "trilogy-brain".into()],
            files: 1234,
            bytes: 76_543_210,
            target: env::TargetProof {
                path: PathBuf::from("/run/media/tango/New Volume/mcpvot-backup"),
                device: "/dev/sda2".into(),
                fstype: "ntfs3".into(),
                is_mountpoint: true,
                free_bytes: Some(336_000_000_000),
            },
            skipped: vec![
                Skip {
                    path: "trilogy-brain/skills/.hub".into(),
                    reason: "dot-cache".into(),
                },
                Skip {
                    path: "trilogy-brain/skills/omarchy".into(),
                    reason: "symlink".into(),
                },
                Skip {
                    path: "default/.env".into(),
                    reason: "secret".into(),
                },
            ],
        };
        let text = serde_json::to_string(&m).unwrap();
        let back: Manifest = serde_json::from_str(&text).unwrap();
        assert_eq!(back, m);
        assert!(text.contains("\"device\":\"/dev/sda2\""));
    }

    /// Pruning keeps the newest N, is a no-op at 0, and can never delete the run
    /// that is being written right now.
    #[test]
    fn prune_keeps_newest_n_and_never_the_current_run() {
        let root = std::env::temp_dir().join(format!("hug-prune-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let names = [
            "pre-update-20260101-000000",
            "pre-update-20260102-000000",
            "pre-update-20260103-000000",
            "pre-update-20260104-000000",
        ];
        for n in names {
            fs::create_dir_all(root.join(n)).unwrap();
        }
        let current = root.join("pre-update-20260104-000000");

        // keep = 0 keeps everything (today's behaviour).
        prune(&root, 0, &current, true);
        assert_eq!(list_runs(&root).len(), 4);

        prune(&root, 2, &current, true);
        let left: Vec<String> = list_runs(&root)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            left,
            vec![
                "pre-update-20260103-000000".to_string(),
                "pre-update-20260104-000000".to_string()
            ]
        );

        // A target that failed its mount proof deletes nothing.
        fs::create_dir_all(root.join("pre-update-20260105-000000")).unwrap();
        prune(&root, 1, &root.join("pre-update-20260105-000000"), false);
        assert_eq!(list_runs(&root).len(), 3);

        let _ = fs::remove_dir_all(&root);
    }

    /// A dry run must not create the target directory or the run directory.
    #[test]
    fn dry_run_writes_nothing() {
        let h = home("dry");
        let dir = run(&h, true, &cfg(&h)).unwrap();
        assert!(!dir.exists(), "dry-run created {}", dir.display());
        let _ = fs::remove_dir_all(&h);
    }

    /// The two roots are genuinely separate: run state stays local while the
    /// pre-update payload goes to the configured bulk root.
    ///
    /// A runner whose temp dir is tmpfs (Linux `/tmp`) cannot host a bulk root at
    /// all, because the volatile-filesystem refusal is unconditional. Assert THAT
    /// there instead of skipping, and assert the retention behaviour where a
    /// backup can actually be written, so the test never depends on the machine's
    /// TEMP filesystem.
    #[test]
    fn bulk_root_receives_only_pre_update_runs() {
        let h = home("roots");
        let external = h.join("mnt/backups");
        fs::create_dir_all(&external).unwrap();
        let mut c = cfg(&h);
        c.bulk_root = external.clone();
        c.allow_local_backup = true; // the temp dir is not a real mount

        if env::target_is_volatile(&external) {
            let err = format!("{:#}", run(&h, false, &c).unwrap_err());
            assert!(
                err.contains("RAM-backed"),
                "a volatile bulk root must be refused with the RAM reason, got: {err}"
            );
            let _ = fs::remove_dir_all(&h);
            return;
        }

        let dir = run(&h, false, &c).unwrap();
        assert!(dir.starts_with(&external));
        assert!(!h.join("backups").join(dir.file_name().unwrap()).exists());
        let _ = fs::remove_dir_all(&h);
    }

    /// Config::new is what main uses; pin that the flag wins over the env var so
    /// a stale env entry cannot silently redirect a deliberate `--backup-root`.
    #[test]
    fn config_new_prefers_the_flag() {
        let h = home("confignew");
        let c = Config::new(
            &h,
            ConfigInput {
                backup_root: Some(PathBuf::from("/mnt/chosen")),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(c.bulk_root, PathBuf::from("/mnt/chosen"));
        assert!(!c.bulk_is_control());
        let _ = fs::remove_dir_all(&h);
    }
}
