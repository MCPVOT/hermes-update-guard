# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-10-16

### Added

- `preflight` now scans `logs/update_receipts/*.json` and reports every receipt
  left in `running` / `interrupted` state, with the pid, start time and the
  version it was pulling. This is the exact "a build step died mid-run and left a
  stale receipt" failure the tool exists for, and it is the reason every later
  Hermes launch keeps retrying the work it owes. `update`'s preflight gate warns
  on the same condition.
- `verify` now checks **local-patch restoration**: a clean working tree *with* git
  stash entries still present is reported as FAIL, because the updater parked your
  patched core file instead of re-applying it. It normally stashes, pulls, then
  restores on top — this proves that actually happened.
- `export` now refreshes the aggregate `golden.json` (the same shape the
  PowerShell exporter writes) alongside `last-known/`, keeping the previous file
  as `golden.json.bak-<stamp>`, so `--baseline golden` stays current.

### Fixed

- **`diff`/`preflight` compared against the wrong baseline shape.**
  `golden.json` is an *aggregate-by-profile* map (`{elevate: {...}, mcpvot: {...}}`),
  but every file baseline was read as one flat surface. Each profile was therefore
  compared against a wrapper object and all twelve surface keys were reported as
  drift with `BASE=null` — on a live install that produced 7 profiles x 12 keys of
  false drift and blocked a clean update. Baselines are now resolved per profile
  across all three shapes in the wild: a per-profile directory (`last-known/`), an
  aggregate-by-profile file (`golden.json`), and a flat single-profile file
  (explicit `--baseline <path>`). An aggregate that does not list a profile is
  "no baseline yet", not a drift storm.
- **The venv-holder gate was blind.** `hermes update --list-venv-holders` exits
  `3` when holders are present, but the old code treated any non-zero exit as
  "free" and returned an empty list — so a live session could slip straight
  through the guard that exists to stop exactly that. Exit 3 is now parsed as
  data, and the probe's exit code is surfaced in `preflight`.
- **A baseline that could not be parsed was silently skipped.** The PowerShell
  exporter writes `last-known/*.json` with a UTF-8 BOM; `serde_json` fails on a
  leading BOM and the old code answered the failure with a silent `continue`,
  i.e. "no drift" when the baseline was unreadable. JSON is now read
  BOM/CRLF-tolerantly, and an unparseable baseline is reported as an explicit
  `baseline UNREADABLE` line that **fails** the check — a broken baseline can
  never read as clean again.
- When Hermes is already mid-update it answers the venv probe with prose instead
  of the documented JSON list. That is now reported as
  "venv state UNKNOWN — treat as held" rather than being mistaken for a free venv,
  and `update` refuses on it.
- `verify`'s "profile(s) drifted" summary counted every drift *line* instead of
  the number of profiles that drifted.
- Removed the now-unused `venv_holders` wrapper (dead code rejected by
  `clippy -D warnings`).

## [0.1.1] - 2026-10-13

### Added

- `vibe` subcommand: MCPVOT identity banner (Doom figlet), plus a compact MCPVOT
  header on `preflight` and `update` output.
- Vibe tests: the banner stays pure ASCII and has no stray trailing newline.
- `CONTRIBUTING.md`; README badge row (CI, Release, License, Rust).

### Fixed

- **CI / release builds:** `rust-toolchain.toml` now uses the canonical
  `[toolchain]` table so `rustup` parses it on the GitHub runners — this
  unblocked all three release builds (Windows/Linux/macOS).
- **CI trigger:** the workflow now runs on `master` as well as `main`, so pushes
  to the default branch are actually tested.
- `update` now returns immediately on a failed `hermes update` instead of running
  post-verify over a broken install; it prints the restore path and leaves
  `verify` to be run only after recovery.
- `--close-desktop` kill path is now cross-platform (`taskkill` on Windows,
  `pkill` on POSIX) and bails loudly when the kill command itself fails.
- Removed dead `sha256_via_cmd` (PowerShell-only); pure-Rust `sha2` manifest
  hashing is now used everywhere.
- Removed dead PowerShell-only helpers; POSIX builds use `pgrep` for the process
  probe.
- Docs made accurate: exit codes (`1` runtime/drift/backup/verify failure, `2`
  clap usage errors), the release workflow drafts the GitHub Release for review
  instead of publishing immediately, and the layout section includes `vibe.rs`.

## [0.1.0] - 2026-10-13

### Added

- `preflight` read-only state check: version, git HEAD/behind/dirty/stashes, venv
  holders, stale update markers, desktop app running, provider drift summary,
  gateway pid/state.
- `export` provider-surface snapshot per profile (12 protected keys, secrets
  masked, SHA-256 manifest) with `last-known` pointer advancement.
- `diff` live-vs-baseline (golden default, `--baseline last-known`/path), exits 1
  on any drift.
- `backup` WAL-safe SQLite online-backup of every profile `state.db`, plus
  config/.env/auth/gateway/channel files and content dirs; `integrity_check` +
  message counts; completeness gate refuses incomplete backups; git state
  captured (HEAD, working-tree patch, stash list/patch, version).
- `update` guarded orchestration: preflight gate (venv holders, desktop app) →
  export → backup → `hermes update --yes` → verify; `--close-desktop` and
  `--force` flags.
- `verify` post-update verification: HEAD, version, per-profile DB integrity,
  drift vs pre-update export, gateway state.
- Home resolution that handles a live-session `HERMES_HOME` pointing at a
  *profile* dir, a `--home` override, Windows `%LOCALAPPDATA%\hermes`, and POSIX
  `~/.hermes`.
- Cross-platform process probes: `tasklist` on Windows, `pgrep` elsewhere.
- Unit tests: API-key masking, surface-key stability, YAML extraction,
  dotted-path lookup.
- GitHub Actions CI (fmt, clippy, test, release build on Windows/Linux/macOS) and
  a release workflow for tagged binaries.
- Hermes skill wrapper under `skills/`.

### Notes

- Windows builds are static-CRT (`-C target-feature=+crt-static`) so the exe runs
  standalone; Linux/macOS keep the platform default.
- Design mirrors the original `update-hermes.ps1` and
  `hermes-provider-drift.ps1` semantics; `hermes update --yes` is invoked via the
  official CLI, never reimplemented.

[Unreleased]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/MCPVOT/hermes-update-guard/releases/tag/v0.1.0
