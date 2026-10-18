---
name: hermes-update-guard
description: Use when running or verifying a `hermes update`, or after a failed update left a stale lock — drives the guarded pipeline (preflight, WAL-safe backup, provider-drift tripwire, post-verify) through the hermes-update-guard binary.
version: 0.3.0
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
| Pin the known-good surface | `hermes-update-guard export` |
| Prove no drift | `hermes-update-guard diff --baseline last-known` |
| WAL-safe backup | `hermes-update-guard backup` |
| Guarded update (all-in-one) | `hermes-update-guard update --close-desktop` |
| Unattended update (survives a closed shell) | `hermes-update-guard update --close-desktop --detached` |
| Watch a run live | `hermes-update-guard watch` (or `watch --no-tui` for a snapshot) |
| Verify after | `hermes-update-guard verify` |
| Identity banner | `hermes-update-guard vibe` |

## Procedure

1. **Confirm the binary is available** (see *Binary availability* below).
2. **Run the ritual in order** — adjust `GUARD` to the absolute path if the
   binary is not on `PATH`:

   ```bash
   GUARD=hermes-update-guard

   $GUARD preflight                        # read-only: any surprises?
   $GUARD export                           # pin known-good provider surface
   $GUARD diff --baseline last-known       # prove clean (exit 0)
   $GUARD backup                           # WAL-safe snapshot of every profile
   $GUARD update --close-desktop           # preflight -> export -> backup -> update -> verify
   $GUARD verify                           # post-update integrity + drift
   ```

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
  `interrupted`, `finished_at=null`) → **an update is already mid-flight**; a real
  `hermes update` is owed before anything else. Never start a second one.
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
- The profile fleet is discovered from the home on disk, not hardcoded, so the
  same binary works on a host with a different profile set (a droplet running
  `hoodbot/sentinel/solana/storemoat`). The hermes CLI is resolved through the
  venv, then `~/.local/bin` and `/usr/local/bin`, then `PATH`.

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
