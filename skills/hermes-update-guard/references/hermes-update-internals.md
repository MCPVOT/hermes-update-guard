# Hermes update internals

Operator-facing depth for `hermes-update-guard`. Load this when a preflight or
diff result looks wrong, or when an update left state behind, or when deciding
whether an update is actually due.

## Baseline shapes — the #1 source of a bogus drift report

| Where | Shape | Read it as |
|---|---|---|
| `backups/provider-surface/golden.json` | **aggregate-by-profile** `{profile: {surface}}` | index by profile name first |
| `backups/provider-surface/last-known/<profile>.json` | flat surface, one file per profile | per-profile |
| `backups/provider-surface/<stamp>/` | per-profile directory (export snapshot) | per-profile |
| explicit `--baseline <path>` | flat single-profile surface | whole file |

Comparing an aggregate-by-profile file as a flat surface yields `BASE=null` for
every key and reports every profile as drifted. Always compare within one shape.

Two writers produce these files, so their shapes must be reconciled before any
comparison: the Rust `export` writes flat per-profile JSON, the PowerShell
exporters write the aggregate `golden.json` plus flat `last-known/`.

PowerShell's `ConvertTo-Json` writes a **UTF-8 BOM** and CRLF; strict JSON
parsers reject a leading BOM, so strip it before parsing.

## `hermes update` flag surface

`--check` (is anything new), `--plan` (read-only: install kind, every running
service per profile with its supervisor and code version, and the restart plan —
safe on a live fleet), `--list-venv-holders` (JSON list; exit **0 = free, 3 =
holders present**), `--yes`, `--backup` / `--no-backup`, `--keep-stash`,
`--branch`, `--switch-branch`, `--force`, `--force-venv`, `--set-channel`,
`--channel`, `--no-gateway-restart`, `--install-id`, `--gateway`.

Stashed-patch contract: local changes are stashed, then **restored on top of the
updated codebase** (log line `Local changes were restored on top of the updated
codebase`). `--keep-stash` parks them in git stash instead — use only
deliberately, then restore by hand.

## Update-log forensics (`logs/update.log`)

Markers worth grepping:

| Line | Meaning |
|---|---|
| `=== hermes update started` | a run began (timestamp in the line) |
| `→ Local changes detected — stashing before update` | your local patch is being parked |
| `Local changes were restored on top of the updated codebase` | patch survived |
| `✓ Code updated!` / `✓ Update complete!` | success, with the new HEAD/version |
| `Update follow-up 'completion' did not finish` | frontend/node-deps build owed; next run retries |
| `The previous update ... was interrupted before it finished (last stage: deps)` | an earlier run died mid-deps |
| `✗ Update failed: git: install failed: [WinError 5] Acceso denegado` | a live process locks the bundled git DLL — retry later, do not kill |

## Channel vs branch

The install tracks a **branch** (`git rev-parse --abbrev-ref HEAD`) and follows an
update **channel** (`stable` / `main`). On `stable`, a local checkout newer than
the latest release waits for the next release even though the upstream branch is
ahead — so "N commits behind" and "no update available" are simultaneously true.
Confirm the channel in preflight output before concluding the install is current.

## Backup layout (`backups/pre-update-<stamp>/`)

Per profile: `.env`, `auth.json`, `channel_directory.json`, `config.yaml`,
`gateway_state.json`, `state.db`, `cron/`, `memories/`, `scripts/`, `skills/`.
Root: `HEAD-before.txt`, `version-before.txt`, `stash-list.txt`,
`stash-0.patch`. Two writers coexist — a directory snapshot and a `.zip`
archive. Treat every one of these as secret-bearing.

## Interrupted-update residue

Home root: `.hermes-update-in-progress` / `.hermes-update-in-progress.lock`.
`backups/update-state-quarantine/<stamp>/` holds the same markers plus the paused
gateway marker once an updater quarantines them. A marker in the root means the
last run did not finish; a marker only in quarantine is normal history.

## Post-update expectations

A completed update restarts the gateways it paused — check `gateway_state.json`
(`pid`, `gateway_state`, served profiles) and re-verify the app afterwards. An
intentionally patched core file survives via the stash-restore path, so confirm
it is still modified after the run and re-apply it if the update dropped it.

