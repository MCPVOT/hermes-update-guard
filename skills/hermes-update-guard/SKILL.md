---
name: hermes-update-guard
description: Use when running or verifying a `hermes update`, or after a failed update left a stale lock — drives the guarded pipeline (preflight, WAL-safe backup, provider-drift tripwire, post-verify) through the hermes-update-guard binary.
version: 0.3.8
author: MCPVOT
license: MIT
platforms: [macos, linux, windows]
metadata:
  hermes:
    tags: [hermes, update, backup, drift, guardian, devops]
    homepage: https://github.com/MCPVOT/hermes-update-guard
    related_skills: [hermes-agent]
    requires_toolsets: [terminal]
---

# hermes-update-guard

Guardian orchestrator for safe `hermes update` runs. It wraps the official update
CLI with a preflight gate, WAL-safe SQLite backups, and a provider-surface
tripwire — the whole scripted update ritual in one static binary.

## When to Use

- Before ANY `hermes update`: run the guarded flow below instead of updating raw.
- After an update: prove nothing drifted (version, DB integrity, provider surface).
- When a previous update failed and left a stale lock, a stale `running` receipt,
  or a silently stashed core file.

## Quick Reference

| Task | Command |
|---|---|
| Peek before updating | `hermes-update-guard preflight` |
| Snapshot the current surface | `hermes-update-guard export` |
| **Approve** the current surface as the golden reference | `hermes-update-guard bless` |
| Prove no drift | `hermes-update-guard diff --baseline last-known` |
| WAL-safe backup | `hermes-update-guard backup` |
| Guarded update (all-in-one) | `hermes-update-guard update --close-desktop` |
| Unattended update (survives a closed shell) | `hermes-update-guard update --close-desktop --detached` |
| Watch a run live | `hermes-update-guard watch` (or `watch --no-tui` for a snapshot) |
| Watch it in a pop-up window | `update` opens it automatically; `hermes-update-guard dashboard` opens it on the last run; `--no-dashboard` suppresses it (cron/ssh) |
| Verify after | `hermes-update-guard verify` |
| Clear stale update markers | `hermes-update-guard quarantine` (plan) / `--apply`; add `--receipts` for dead receipt files |
| Record a profile as gone on purpose | `hermes-update-guard retire <profile>` (undo: `--undo`) |
| Transcript / context-health audit | `hermes-update-guard health` |
| One profile in depth | `hermes-update-guard health --profile <name>` |
| MCP readiness (OAuth tokens, headless auth) | `hermes-update-guard mcp` |
| Fleet log-noise signatures | `hermes-update-guard health --logs` |
| Snapshot log signatures before an update | `hermes-update-guard snapshot-logs` |
| Is the binary I am running current? | `hermes-update-guard self-check` |
| Identity banner | `hermes-update-guard vibe` |

## Procedure

1. **Confirm the binary is available** (see *Binary availability* below).
2. **Run the ritual in order** — adjust `GUARD` to the absolute path if the
   binary is not on `PATH`:

   ```bash
   GUARD=hermes-update-guard

   $GUARD self-check                      # is the binary I am running the current one?
   $GUARD preflight                       # read-only: any surprises?
   $GUARD health                          # transcript health: duplicate live rows, bloat, at-risk sessions
   $GUARD export                          # snapshot the surface (does NOT move the approved baseline)
   $GUARD diff --baseline last-known      # prove clean (exit 0)
   $GUARD backup                          # WAL-safe snapshot of every profile
   $GUARD update --close-desktop          # preflight -> export -> backup -> update -> verify
   $GUARD verify                          # post-update integrity + drift
   ```

   **`export` does NOT bless.** It snapshots the surface, advances `last-known/` and
   writes the `last-export.txt` pointer. The APPROVED reference (`golden.json`) moves
   only when a human runs `bless` — a routine export must never adopt whatever the
   surface happened to be, or a silent rewrite becomes the new approved state (that is
   exactly how the 2026-10-11/12 unknown-writer rewrite went unnoticed for two days).

   In an agent session (or anywhere the caller may time out), use
   `update --close-desktop --detached`: it starts the update in its own process
   group and returns immediately, then `$GUARD watch` renders progress. A
   finaliser killed by a caller's timeout is what leaves the stale markers that
   block every later update.

3. **Apply the decision rules** (below) at each gate before continuing.

## Binary availability

The expected binary is `hermes-update-guard` on `PATH` (or `target/release/` of a
local build). If it is not installed:

- Build it: `cargo install --path <repo>` from the hermes-update-guard repo.
- Or grab the per-OS artifact from the repo's GitHub Releases.

## Decision rules

- `preflight` reports `dirty: N file(s)` → those files will be stashed by the
  updater. `verify` afterwards proves they came back: a clean tree **with** stash
  entries still present is a FAIL (the patched file was parked, not restored).
  `hermes_cli/models_catalog_static.py` is the usual such patch.
- `preflight` → `[update receipts]` shows an unfinished receipt (`running` /
  `interrupted`, `finished_at=null`): check the **pid**. `pid dead` = a stale receipt,
  history, nothing in flight — the real update it owed was completed later, so it
  does NOT block. `pid LIVE` = an update is genuinely running; never start a second.
  Dead ones are a *warning* (0.3.7+) and `quarantine --receipts --apply` moves them.
- `health --profile <name>` is the **readiness** view for one profile: model/provider,
  the credential that provider actually uses (resolved through the profile's own
  `providers.<name>.api_key_env` when it is a custom provider), fallback-chain
  length, the compression engine, enabled MCP servers (and which are OAuth — the
  v0.22.2 CLI-startup hang class), and the transcript's duplicate/bloat/stuck
  counters. **A profile with `fallbacks: 0` has no resilience** — a rate-limited
  or dead primary ends the turn, which is exactly how `default` and `polloriko`
  went down on 402/429.
- `health` reports **`dupUIDs > 0`** (the same logical message stored twice as live
  rows) or a **bloat ratio** ≫ 1 → the transcript sent to the model is inflated, so
  sessions hit the context wall early. `at risk` / `largest live` lines show which
  sessions are closest to a 1M-token window. Report it; do not "fix" it by deleting
  rows — repairing a transcript needs the gateway stopped and a verified backup.
- `mcp` is the **readiness** view (also printed at the end of `preflight`): per
  profile, which servers are enabled, their auth type, and whether an OAuth token is
  present, expired, or unreadable. Only `auth: oauth` servers can fail headlessly —
  a gateway or cron job can never complete a browser authorisation, so an expired or
  never-registered token fails every call and reads as a slow profile. `auth: none`
  / stdio servers have no token file by design and must never be flagged.
- `health --logs` and `snapshot-logs` add the **log-signal** view: errors grouped
  into stable signatures by shape (ids and numbers folded), so a repeating failure
  counts once. `update` snapshots the counts before the update and `verify` reports
  only signatures that are NEW, which attributes a regression to that update instead
  of guessing afterwards.
- `preflight` prints the **resolved update TARGET**. `TARGET : none - this update
  would be a NO-OP` means the channel's newest release is already installed: the
  repo can still be N commits behind origin/main, but on a release channel that
  does not mean an update is pending.
- `health` reports a **stuck session** (a recorded `compression_failure_error`) → that
  session's summariser route is dead (quota/credits). Compaction cannot summarise
  until the route works; a custom `context.engine` that does not summarise makes it
  worse.
- `self-check` flags a **stale installed binary** (PATH binary older than the repo
  build, or a version mismatch) or a **clone under the Hermes `tools/` dir** → a
  `cargo install` from that clone reinstalls the stale build, and a stale guard
  reports a clean preflight for a version that no longer exists. Reinstall before
  trusting any verdict.
- `preflight` → `[venv holders]`:
  - `probe exit 3` = holders present, with pid/kind/exe listed. Wait for them.
  - `venv state UNKNOWN` = hermes answered the probe with prose instead of the
    documented JSON list, which is what it does while an update is already
    running. Treat the venv as held.
- `diff` exits 1 (drift found, or a baseline was `UNREADABLE`) → **do not
  update**; investigate `model.*`, `fallback_*`, `delegation`, and
  `display.language` first.
- Baselines come in three shapes and all are understood: `golden.json` (one
  object per profile), `last-known/` (one flat file per profile), and an explicit
  `--baseline <path>` (one flat surface). An aggregate that does not list a
  profile means "no baseline yet", not drift.
- `update` refuses to run while a venv is held or its state is unknown, or while
  the Desktop app is running; pass `--close-desktop` only when the operator has
  already closed their session.
- Backup FAIL → never update from an unverified backup.
- `verify` is **fail-closed**: any check it cannot perform (unreadable HEAD,
  missing hermes CLI, no database found, missing baseline) is a failure, and the
  process exits `1`. Read the exit code, never just the last printed line.
- `verify` **attributes** context-health problems: the run captures a health
  snapshot before the update, so an unchanged condition is reported as
  PRE-EXISTING (a warning) and only a regression fails the verdict. A standalone
  `verify` with no baseline in the run state reports the full table instead.
- `verify` also surfaces the newest **update receipt** (kind, outcome, pid) and
  says plainly whether a recorded "interrupted" attempt is still live or is
  history superseded by a later successful run.
- `backup` has a **disk-space gate**: it measures the fleet's databases, asks for
  1.5x + 512 MB, and refuses before copying if the volume cannot take it. The
  fleet's DBs are large (one profile was measured at 2.0 GB; the fleet at
  4.7 GB, ~5.5 GB needed), so a backup is not a small operation.
- `gateway_state.json` is not proof of life — `verify` checks the pid.
- The profile fleet is discovered from the home on disk, not hardcoded, so the
  same binary works on a host with a different profile set (a droplet running
  `hoodbot/sentinel/solana/storemoat`). The hermes CLI is resolved through the
  venv, then `~/.local/bin` and `/usr/local/bin`, then `PATH`.

### What a normal run looks like (measured, 6 profiles)

Run `20261021-201542`, Windows, 4.7 GB of `state.db` across the fleet:

| stage | time | note |
|---|---|---|
| preflight | 36 s | |
| export | 0.2 s | |
| backup | 2 m 31 s | 26 files + 6 DBs, integrity-checked; needs ~5.5 GB |
| **update** | **25 m 21 s** | 78% of the run — `hermes update` itself |
| verify | 3 m 41 s | |

Inside those 25 minutes the budget is Hermes's, not the guard's: git
fetch/stash/pull ≈5 min, node deps ≈4 min, **web UI + desktop build ≈14 min**.
Expect ~30 minutes; a long UPDATE stage is not a hang.

Expected noise on Windows that is **not** a fault:

- `Windows gateway restart could not be verified … #48820` and
  `Update follow-up 'gateway_restart' did not finish`. The updater respawns its
  own gateway with `--replace`, killing the one it just started. The guard's
  `-- GATEWAY RECOVERY --` block is the authority: it requires the same pid alive
  on two probes and waits up to 120 s. Never restart the gateway in parallel.
- `Quick snapshot skipped state.db: exceeds … limit` — Hermes' own snapshot
  refuses any DB over 1 GB (hardcoded), so the guard's backup is the only restore
  point for a profile like `vot`.

### What exit 0 does and does not mean

Without `--detached`, exit 0 means every stage ran and `verify` returned Ok.

With `--detached`, exit 0 means **only that the child process spawned**. The real
verdict is the child's: read `backups/guard-run.json` (`exit_code`, `blockers`,
`warnings`) or run `verify` afterwards. Non-empty `blockers` = failure;
non-empty `warnings` = noted-and-continued, still a PASS.

## Pitfalls

- **Updating with the desktop app open** → the update refuses; close the app (or
  pass `--close-desktop`) and retry.
- **Ignoring a `diff` drift** → you lose the guarantee that the update keeps your
  model/provider config; resolve drift first.
- **Trusting a partial backup** → the `backup` gate already refuses incomplete
  backups; do not override it.
- **Stale lock after a previous failed update** → `preflight` surfaces it; clear
  it before retrying.
- **Assuming a free venv** → `hermes` exits `3` when holders exist. Anything
  other than an empty list means "wait", not "go".
- **Trusting a `[]` that is really prose** → while an update runs, hermes answers
  the venv probe with a sentence instead of JSON. Preflight flags that as
  `venv state UNKNOWN`; treat it as held.
- **False drift from a shaped baseline** → `golden.json` holds one object per
  profile. Comparing a profile against the whole aggregate reports every key as
  drift with `BASE=null`. Baselines are resolved per profile for this reason.
- **A baseline that fails to parse saying "no drift"** → the PowerShell exporter
  writes a UTF-8 BOM, and a silent skip reads as clean. Unreadable baselines are
  now reported and fail the check.
- **Trusting `verify`'s last line instead of its exit code** → in versions before
  0.3.0 `verify` printed `PASS` and exited `0` even with no `hermes-agent`, an
  unreadable HEAD and no venv. The exit code is the verdict; a `grep PASS` is not.
- **Reading a clean tree as "nothing was pinned"** → a clean tree with no stash
  entries is also what a *dropped* patch looks like. `verify` now compares against
  the file list in the newest `pre-update-*/working-tree.patch`.
- **Quarantining an empty `.lock` while an update is live** → the empty lock has no
  pid, so judging it on its own says "dead". `quarantine` treats the markers as a
  SET: if any owner is live *or the probe cannot answer*, nothing is moved.
- **Trusting the fleet to show a missing profile** → the fleet comes from disk, so a
  deleted half-restored profile just stops appearing. `self-check` compares against
  every `pre-update-*` backup and reports profiles that were there but are gone,
  with the newest stamp that saw them.
- **Treating a deliberately deleted profile as an accident forever** → an
  intentional deletion and an accidental one look identical on disk. If you removed
  a profile on purpose, `retire <name>` it (recorded in
  `backups/profile-retired.json`); otherwise `self-check` keeps flagging it. `retire`
  refuses a profile that still exists, so it cannot be used to hide a live one.
- **Running a stale guard** → `self-check` resolves the binary on PATH, compares it
  with the repo build and flags stale clones, because a stale guard reports a clean
  preflight for a version that no longer exists.
- **Assuming `export` approved the surface** → it does not. `export` only
  snapshots; the approved reference moves on `bless` alone. An export that
  re-blessed golden would quietly adopt a rogue rewrite as the new approved state.
- **Trusting a guard you did not check the version of** → a stale binary on PATH
  (older than the repo build, or a different version) will happily print a clean
  preflight. Run `self-check` first; it is the check that would have caught the
  2026-10-19 "the tool guarding it was not the one you were running" incident.
- **Reading `gateway_state.json` as proof the gateway is alive** → the file
  outlives the process it describes. `verify` now checks the pid; a recorded
  "running" gateway with a dead pid is a FAIL, not a warning to ignore.

## Maintaining the tool itself

Nothing above is about changing the binary. Load:

| Task | File |
|---|---|
| Fix / improve / release the guard: gates, CI matrix, branch protection, release + publish, version bumps, reviewing another agent's landing, how to record an operator decision instead of weakening a check | `references/repo-maintenance.md` |
| What the update pipeline actually does underneath | `references/hermes-update-internals.md` |
| The `watch` TUI: layout, palette, read-only contract | `references/tui-watch-view.md` |
| Reading a finished run: measured timings, where the minutes go, the gateway churn, a stale version label | `references/run-forensics.md` |
| The pop-up window (`dashboard/`, separate Tauri crate, excluded from packaging) | the `tauri-desktop-app` skill |

## Manual restore (emergency)

```bash
rsync -a "$HOME/.hermes/backups/pre-update-<stamp>/<profile>/state.db" \
      "<HERMES_HOME>/<profile>/state.db"
```

The same applies to `.env`, `auth.json`, and `config.yaml`. Restore only after
stopping the gateway (`hermes gateway stop`) and closing Hermes.

## Verification

- `hermes-update-guard verify` exits `0` and reports DB integrity `ok` with no
  drift versus the pre-update export.
- Exit codes: `0` ok · `1` drift found, backup incomplete, verify failure, or any
  runtime error · `2` CLI usage error (bad flags/arguments).
