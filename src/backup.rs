use anyhow::{bail, Context, Result};
use chrono::Local;
use rusqlite::OpenFlags;
use std::fs;
use std::path::{Path, PathBuf};

use crate::env;

/// WAL-safe backup of every profile: small files, content dirs (memories/skills/scripts/cron),
/// and a STANDALONE state.db snapshot via the SQLite online-backup API (WAL folded in).
///
/// Port of update-hermes-v2.ps1 phase 3 (H1-H5): online-backup API, integrity_check +
/// message count per db, COMPLETENESS GATE refusing to continue when a profile's db is missing.
pub fn run(home: &Path, dry_run: bool) -> Result<PathBuf> {
    run_with(home, dry_run, &mut |_, _| {})
}

/// Same as [`run`], reporting each profile's database result as it completes so a
/// live viewer can show per-profile progress during the long, silent copy phase.
pub fn run_with(home: &Path, dry_run: bool, on_db: &mut dyn FnMut(&str, bool)) -> Result<PathBuf> {
    let stamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let backup_root = home.join("backups");
    let dir = backup_root.join(format!("pre-update-{}", stamp));

    println!("== PRE-UPDATE BACKUP ({}) ==", dir.display());
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
    match env::free_space_bytes(&backup_root) {
        Some(free) if free < needed_with_headroom => {
            println!(
                "  [FAIL] not enough disk for the backup: need ~{:.1} GB, {} GB free",
                needed_with_headroom as f64 / 1_073_741_856.0,
                free / 1_073_741_856
            );
            anyhow::bail!("insufficient disk space for a verified backup");
        }
        Some(free) => println!(
            "  disk: {} GB free, backup needs ~{:.1} GB",
            free / 1_073_741_856,
            needed_with_headroom as f64 / 1_073_741_856.0
        ),
        None => println!("  disk: free space unknown - proceeding (cannot prove the volume can take it)"),
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

    // 3a. small files + content dirs per profile
    for p in env::profiles(home).iter().map(|s| s.as_str()) {
        let src = env::profile_dir(home, p);
        if !src.is_dir() {
            continue;
        }
        let dst = dir.join(p);
        fs::create_dir_all(&dst)?;
        for f in [
            "config.yaml",
            ".env",
            "auth.json",
            "gateway_state.json",
            "channel_directory.json",
        ] {
            let s = src.join(f);
            if s.is_file() {
                fs::copy(&s, dst.join(f))?;
                total += 1;
                println!("  {} {}", p, f);
            }
        }
        for sub in ["memories", "skills", "scripts", "cron"] {
            let s = src.join(sub);
            if s.is_dir() {
                copy_dir(&s, &dst.join(sub))?;
                println!("  {} {}/ (dir)", p, sub);
            }
        }
    }

    // 3b. databases via SQLite online backup -> standalone, verified
    let mut fails = 0usize;
    for p in env::profiles(home).iter().map(|s| s.as_str()) {
        let src_db = env::state_db(home, p);
        if !src_db.exists() {
            continue;
        }
        // Hermes' OWN pre-update snapshot skips any state.db over 1 GB
        // ("Quick snapshot skipped oversized DB file(s)"). Say so: this guard's
        // copy is then the only restore point that has the file.
        let bytes = crate::context_health::db_size(home, p);
        if bytes > 1_073_741_824 {
            println!(
                "  note {} state.db is {:.1} MB - Hermes' built-in snapshot SKIPS files over 1 GB; this backup is the only copy",
                p,
                bytes as f64 / 1_048_576.0
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
    for p in env::profiles(home).iter().map(|s| s.as_str()) {
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
            &["-C", repo.to_str().unwrap(), "rev-parse", "HEAD"],
            None,
        )
        .ok()
        .map(|(_, h)| fs::write(dir.join("HEAD-before.txt"), h.trim()));
        env::run_capture(
            Path::new("git"),
            &["-C", repo.to_str().unwrap(), "diff"],
            None,
        )
        .ok()
        .map(|(_, d)| fs::write(dir.join("working-tree.patch"), d));
        env::run_capture(
            Path::new("git"),
            &["-C", repo.to_str().unwrap(), "stash", "list"],
            None,
        )
        .ok()
        .map(|(_, s)| fs::write(dir.join("stash-list.txt"), s));
        env::run_capture(
            Path::new("git"),
            &[
                "-C",
                repo.to_str().unwrap(),
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
    println!(
        "backup complete and verified: {} files + {} dbs",
        total,
        db_count(home)
    );
    println!("backup dir: {}", dir.display());
    Ok(dir)
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

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let s = entry.path();
        let d = dst.join(entry.file_name());
        if s.is_dir() {
            copy_dir(&s, &d)?;
        } else {
            fs::copy(&s, &d)?;
        }
    }
    Ok(())
}
