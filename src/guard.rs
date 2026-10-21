use anyhow::{bail, Result};
use std::path::Path;

use crate::backup;
use crate::drift;
use crate::env;
use crate::state::{RunState, StageState};

/// Full guarded update: preflight -> drift export -> WAL-safe backup -> hermes update --yes -> verify.
///
/// Writes `backups/guard-run.json` at every stage transition so `watch` can render
/// the run live without touching the updater.
pub fn run(
    home: &Path,
    close_desktop: bool,
    force: bool,
    detached: bool,
    channel: Option<&str>,
) -> Result<()> {
    if detached {
        return spawn_detached(home, close_desktop, force, channel);
    }

    crate::vibe::print_compact(env!("CARGO_PKG_VERSION"));
    println!("== HERMES UPDATE GUARDIAN ==");

    let mut st = RunState::new(home, env!("CARGO_PKG_VERSION"));
    st.seed_profiles(&env::profiles(home));
    let _ = st.write(home);

    let result = run_stages(home, close_desktop, force, &mut st, channel);

    st.finish(if result.is_ok() { 0 } else { 1 });
    let _ = st.write(home);
    result
}

fn run_stages(
    home: &Path,
    close_desktop: bool,
    force: bool,
    st: &mut RunState,
    channel: Option<&str>,
) -> Result<()> {
    // ---- PREFLIGHT ---------------------------------------------------------
    st.stage("preflight", StageState::Active);
    let _ = st.write(home);

    if !force {
        println!("\n-- PREFLIGHT GATE --");

        // --list-venv-holders exits 3 when holders are present, so the probe's
        // exit code is part of the answer, not an error to be discarded. An
        // unparseable answer (hermes is mid-update) counts as held.
        let (holders, vexit, parsed) = env::venv_holders_probe(home);
        if !parsed || !holders.is_empty() || vexit == 3 {
            let msg = if !parsed {
                format!("venv state UNKNOWN (probe exit {vexit}) - treat as held")
            } else {
                format!("venv held by {} (probe exit {vexit})", holders.join(", "))
            };
            st.blocker(&msg);
            st.stage("preflight", StageState::Failed);
            let _ = st.write(home);
            bail!(
                "{} â€” update would refuse or strand the install. Stop them first.",
                msg
            );
        }

        // Tri-state: `Unknown` (tasklist broken) is NOT "closed". The old bool
        // probe answered false on any error, so a broken tool let the update run
        // while the app still held every profile's state.db.
        const DESKTOP: &str = "Hermes.exe";
        match env::process_state(DESKTOP) {
            env::ProcState::Absent => println!("  Desktop app not running"),
            env::ProcState::Unknown(why) => {
                let msg = format!(
                    "cannot verify whether the Desktop app is running ({why}) - treating it as RUNNING"
                );
                st.blocker(&msg);
                st.stage("preflight", StageState::Failed);
                let _ = st.write(home);
                bail!("{}", msg);
            }
            env::ProcState::Running if !close_desktop => {
                st.blocker("Desktop app is RUNNING (pass --close-desktop)");
                st.stage("preflight", StageState::Failed);
                let _ = st.write(home);
                bail!("Desktop app is RUNNING - pass --close-desktop or close it manually");
            }
            env::ProcState::Running => {
                println!("  Desktop app running - closing it (graceful, then forced)...");
                let report = match env::close_process(
                    DESKTOP,
                    std::time::Duration::from_secs(8),
                    std::time::Duration::from_secs(12),
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        let msg = format!("failed to close the Desktop app: {e}");
                        st.blocker(&msg);
                        st.stage("preflight", StageState::Failed);
                        let _ = st.write(home);
                        bail!("{}", msg);
                    }
                };
                if !report.closed {
                    let pids = if report.survivors.is_empty() {
                        "pids unknown".to_string()
                    } else {
                        format!("pids {:?}", report.survivors)
                    };
                    let msg = format!(
                        "Desktop app {} ({}) - close it from the tray and re-run; an app that respawns itself cannot be killed from here",
                        report.detail, pids
                    );
                    st.blocker(&msg);
                    st.stage("preflight", StageState::Failed);
                    let _ = st.write(home);
                    bail!("{}", msg);
                }
                println!("  Desktop app {}", report.detail);
            }
        }

        for m in [
            ".hermes-update-in-progress",
            ".hermes-update-in-progress.lock",
        ] {
            if home.join(m).exists() {
                // A leftover marker is a warning, not a refusal: hermes quarantines
                // these itself, and blocking on them strands an install forever.
                println!(
                    "  [warn] stale marker {} present - previous update did not finish",
                    m
                );
                st.blocker(&format!("stale marker {m}"));
            }
        }
        let no_cfg = env::profiles_without_config(home);
        if !no_cfg.is_empty() {
            println!(
                "  [warn] {} profile dir(s) have no config.yaml: {}",
                no_cfg.len(),
                no_cfg.join(", ")
            );
            st.blocker(&format!("{} profile(s) without config.yaml", no_cfg.len()));
        }

        let stale = crate::preflight::stale_receipts(home);
        if !stale.is_empty() {
            for s in &stale {
                println!("  [warn] unfinished update receipt: {}", s);
            }
            let live = stale.iter().filter(|l| l.contains("pid LIVE")).count();
            if live == 0 {
                println!(
                    "  [warn] {} stale receipt(s) with dead pids - history, not an update in flight",
                    stale.len()
                );
            } else {
                println!(
                    "  [warn] {} receipt(s) with a LIVE pid - an update is in flight; finish it first",
                    live
                );
            }
            st.blocker(&format!("{} unfinished update receipt(s)", stale.len()));
        }
        println!("  preflight gate passed");
    } else {
        println!("  preflight gate SKIPPED (--skip-preflight)");
        st.blocker("preflight SKIPPED (--skip-preflight)");
    }
    st.stage("preflight", StageState::Done);
    let _ = st.write(home);

    // ---- EXPORT ------------------------------------------------------------
    st.stage("export", StageState::Active);
    let _ = st.write(home);
    println!("\n-- DRIFT EXPORT (pre-update) --");
    drift::export(home)?;
    st.stage("export", StageState::Done);
    let _ = st.write(home);

    // ---- BACKUP ------------------------------------------------------------
    st.stage("backup", StageState::Active);
    let _ = st.write(home);
    println!("\n-- WAL-SAFE BACKUP --");
    let backup_dir = {
        let mut on_db = |profile: &str, ok: bool| {
            st.profile(
                profile,
                if ok {
                    StageState::Done
                } else {
                    StageState::Failed
                },
                if ok {
                    "state.db verified"
                } else {
                    "state.db FAILED"
                },
            );
            let _ = st.write(home);
        };
        backup::run_with(home, false, &mut on_db)?
    };
    st.stage("backup", StageState::Done);
    let _ = st.write(home);

    // Snapshot the fleet's log signatures BEFORE the update. The verify step then
    // reports which error signatures are NEW, so a regression is attributed to this
    // update instead of guessed at afterwards (the same trick that made the
    // context-health verdict honest).
    match crate::logsignals::snapshot(home) {
        Ok(p) => println!("  log-signal snapshot: {}", p.display()),
        Err(e) => println!("  ! log-signal snapshot failed: {}", e),
    }

    // ---- UPDATE ------------------------------------------------------------
    // Capture the fleet's context health BEFORE the update. `verify` compares
    // against it, so a transcript that was already broken is reported as
    // pre-existing instead of being blamed on the update.
    st.health_before = Some(crate::context_health::summarize(home));
    if let Some(hb) = &st.health_before {
        println!(
            "  context health before update: dup_uids={} max_bloat={:.1}x stuck={} at_risk={}",
            hb.dup_uids, hb.max_bloat, hb.stuck, hb.at_risk
        );
    }
    st.stage("update", StageState::Active);
    let _ = st.write(home);
    println!("\n-- HERMES UPDATE --");
    let hermes = match env::resolve_hermes(home) {
        Some(p) => p,
        None => {
            st.blocker("no hermes CLI found");
            st.stage("update", StageState::Failed);
            let _ = st.write(home);
            bail!(
                "no hermes CLI found (venv, PATH, ~/.local/bin, /usr/local/bin) â€” cannot update"
            )
        }
    };
    println!("  using {}", hermes.display());
    // --channel is a TRANSIENT override for this run only (hermes keeps the
    // persisted channel untouched), so a guarded run can pull the newest main
    // without silently moving the install off its release channel.
    let mut upd_args: Vec<&str> = vec!["update", "--yes"];
    if let Some(ch) = channel {
        println!("  channel override for this run only: {}", ch);
        upd_args.push("--channel");
        upd_args.push(ch);
    }
    let (code, out) = env::run_capture(&hermes, &upd_args, None)?;

    if !out.is_empty() {
        println!("{}", out);
    }
    if code != 0 {
        println!("\n*** UPDATE FAILED (exit {}) ***", code);
        println!("    backup retained at: {}", backup_dir.display());
        println!("    run `hermes-update-guard verify` only after you restore/harden");
        st.blocker(&format!("hermes update failed (exit {code})"));
        st.stage("update", StageState::Failed);
        let _ = st.write(home);
        return Err(anyhow::anyhow!(
            "hermes update failed (exit {}) - backup at {} for restore",
            code,
            backup_dir.display()
        ));
    }
    println!("  hermes update exit 0");
    st.stage("update", StageState::Done);
    let _ = st.write(home);

    // ---- GATEWAY RECOVERY --------------------------------------------------
    // The updater stops the gateway to swap the code, and it REFUSES to restart
    // it when the checkout has a locally modified file ("git left half-written").
    // The result is an install whose gateway is DOWN - the single most common
    // post-update failure, and the reason "the update succeeded but Hermes is
    // dead" kept happening. Recover it here, then verify.
    println!("\n-- GATEWAY RECOVERY --");
    match crate::verify::gateway_status(home) {
        Some(g) => {
            let alive = matches!(env::pid_running(g.pid), env::ProcState::Running);
            if !alive {
                println!(
                    "  gateway pid {} is not running (state={:?}) - restarting it",
                    g.pid, g.state
                );
                st.blocker(&format!("gateway pid {} was down after the update", g.pid));
                if let Some(h) = env::resolve_hermes(home) {
                    let (code, out) = env::run_capture(&h, &["gateway", "restart"], None)
                        .unwrap_or((-1, String::new()));
                    for l in out.lines().take(8) {
                        println!("    {}", l.trim());
                    }
                    if code != 0 {
                        println!("  ! `hermes gateway restart` exited {}", code);
                    }
                }
                // give it a moment to come up, then re-read the state
                for _ in 0..15 {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    if let Some(g2) = crate::verify::gateway_status(home) {
                        if matches!(env::pid_running(g2.pid), env::ProcState::Running) {
                            println!("  gateway back up: pid={} state={}", g2.pid, g2.state);
                            break;
                        }
                    }
                }
            } else {
                println!("  gateway alive (pid={})", g.pid);
            }
        }
        None => println!("  no gateway_state.json - nothing to recover"),
    }
    let _ = st.write(home);

    // ---- VERIFY ------------------------------------------------------------
    st.stage("verify", StageState::Active);
    let _ = st.write(home);
    println!("\n-- POST-UPDATE VERIFY --");
    let verified = crate::verify::run(home);
    match &verified {
        Ok(()) => st.stage("verify", StageState::Done),
        Err(e) => {
            st.stage("verify", StageState::Failed);
            st.blocker(&format!("verify: {e}"));
        }
    }
    let _ = st.write(home);
    verified?;

    println!(
        "\nUPDATE FLOW COMPLETE - backup at {}",
        backup_dir.display()
    );
    Ok(())
}

/// Run the update in its own process, detached from this terminal.
///
/// Two of the historical update failures were finalisers killed by a caller's
/// timeout, each leaving a marker that blocked every later command. A detached
/// child outlives the shell that started it, so a closed lid or a timed-out agent
/// call can no longer kill an update halfway through its build.
fn spawn_detached(
    home: &Path,
    close_desktop: bool,
    force: bool,
    channel: Option<&str>,
) -> Result<()> {
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe()?;
    let log = home.join("backups").join("guard-update.log");
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::File::create(&log)?;

    let mut cmd = Command::new(&exe);
    cmd.arg("update");
    cmd.arg("--home").arg(home);
    if close_desktop {
        cmd.arg("--close-desktop");
    }
    if force {
        cmd.arg("--force");
    }
    if let Some(ch) = channel {
        cmd.arg("--channel").arg(ch);
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::from(file.try_clone()?));
    cmd.stderr(Stdio::from(file));

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
        cmd.creation_flags(0x0000_0008 | 0x0000_0200);
    }

    let child = cmd.spawn()?;
    crate::vibe::print_compact(env!("CARGO_PKG_VERSION"));
    println!("== HERMES UPDATE GUARDIAN (detached) ==");
    println!("  child pid : {}", child.id());
    println!("  log       : {}", log.display());
    println!("  state     : {}", crate::state::state_path(home).display());
    println!("\n  watch it  : hermes-update-guard watch");
    println!("  snapshot  : hermes-update-guard watch --no-tui");
    Ok(())
}
