---
name: hermes-update-guard
description: Use before running `hermes update` — orchestrates the update with preflight, WAL-safe backup, provider-drift tripwire, and post-verify via the hermes-update-guard binary.
version: 0.1.0
author: hermes-update-guard
license: MIT
platforms: [darwin, linux, windows]
tags: [hermes, update, backup, drift, guardian]
---

# hermes-update-guard

Guardian orchestrator for safe `hermes update` runs. It wraps the official
update CLI with a preflight gate, WAL-safe SQLite backups, and a provider-surface
tripwire — exactly the scripted ritual, in one static binary.

## When to use

- Before ANY `hermes update`: run the guarded flow below.
- After an update, to prove nothing drifted (version, DB integrity, provider surface).

## Binary availability

The expected binary is `hermes-update-guard` on PATH (or `target/release/` of a
local build). If it is not installed:

- Build: `cargo install --path <repo> -- locked` from the hermes-update-guard repo.
- Or grab the per-OS artifact from GitHub Releases.

## Ritual

```bash
GUARD=hermes-update-guard               # adjust to absolute path if needed

$GUARD preflight                        # read-only: surprises?
$GUARD export                           # pin known-good provider surface
$GUARD diff --baseline last-known       # prove clean, exit 0
$GUARD backup                           # WAL-safe snapshot of every profile
$GUARD update --close-desktop           # preflight -> export -> backup -> update -> verify
$GUARD verify                           # post-update integrity + drift
```

## Decision rules

- `preflight` reports `dirty: N file(s)` → those will be stashed by the
  updater. If a tracked core file is intentionally patched (e.g.
  `hermes_cli/models_catalog_static.py`), verify the stash list after update
  and restore it if the update dropped it.
- `diff` exit 1 (drift found) → DO NOT update; investigate `model.*`,
  `fallback_*`, `delegation`, `display.language` first.
- `update` refuses while a venv holder or the Desktop app is running; pass
  `--close-desktop` only when the operator already closed their session.
- Backup FAIL → never update from an unverified backup.

## Manual restore (emergency)

```bash
rsync -a "$HOME/.hermes/backups/pre-update-<stamp>/<profile>/state.db" \
      "<HERMES_HOME>/<profile>/state.db"
```
Same for `.env`, `auth.json`, `config.yaml`. Restore only after stopping the
gateway (`hermes gateway stop`) and closing Hermes.

## Exit codes

`0` ok · `1` drift/backup/verify failure · `2+` hard error.