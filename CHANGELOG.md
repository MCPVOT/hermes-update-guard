# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.2] - 2026-10-21

### Fixed

- **CI was red:** `cargo fmt --all -- --check` failed across 25 positions left by
  the 0.3.1 work, so every push would have failed the `check` job. Formatted.
- **`quarantine` could move a live update's marker.** Only a marker that carries a
  pid could be judged live, and an empty `.lock` (the most common leftover) has no
  pid at all — so it was always movable. Markers are now treated as a SET: if any
  owner in the home is live *or unverifiable*, nothing is moved. `ProcState::Unknown`
  is a hold, not a pass.
- **A profile could vanish without a word.** The fleet is discovered from disk, so a
  deleted or half-restored profile simply stops appearing and every command still
  reports success. `self-check` now scans every `pre-update-*` backup and reports
  profiles that were present then but are absent now, with the newest stamp that
  saw them. Measured on the live install: `elevate (last seen 20261016-135956)`.
- **Profile directories without `config.yaml` were dropped from the fleet**, so a
  half-restored profile was backed up by nobody and verified by nobody. They are
  now part of the fleet (their `state.db` is still backed up) and reported as an
  anomaly by `self-check` and by `update`'s preflight.

### Notes

- `PROFILES` is now `["default"]` — a last-resort fallback only. The binary ships
  publicly and must not carry one operator's profile names; the real fleet is
  enumerated from disk.
- 34 tests (was 29).

## [0.3.1] - 2026-10-19

### Fixed

- **The process probe failed open.** `process_running() -> bool` answered `false`
  when `tasklist` could not run, exited non-zero, or printed anything it did not
  understand — so a broken probe read as "the Desktop app is closed" and the
  update proceeded against a locked `state.db`. The probe is now tri-state
  (`Running` / `Absent` / `Unknown`) and `Unknown` blocks the preflight.
- **`--close-desktop` gave up after one forced kill and a fixed 6 s sleep.**
  It now asks gracefully, waits, escalates to a forced stop, waits again, and on
  failure reports the surviving PIDs instead of "still running after kill".
- **`PROFILES` shipped one operator's profile names** (including client names) in
  a public binary. The fallback is now `["default"]`.

### Added

- `preflight` prints the guard's own version and binary path, so a stale
  `~/.cargo/bin` copy cannot masquerade as the current build.
- Unit tests for the CSV parser (exact image match, prose rejection) and for the
  bounded wait (unknown, timeout, exit).

## [0.3.0] - 2026-10-17

### Added

- `watch` — a live, read-only view of a run (ratatui): a stage rail
  (preflight/export/backup/update/verify), a per-profile matrix so the long silent
  backup phase is no longer a dead cursor, a blockers band, and the guard's log
  tail coloured by severity. `watch --no-tui` prints a one-shot snapshot for
  scripts and CI. The viewer reads `backups/guard-run.json` and never writes state,
  so a viewer crash cannot strand an install, and it works over SSH on a droplet.
- `update --detached` — the update runs in its own process group, detached from
  the calling shell, with its output in `backups/guard-update.log`. Two of the
  historical failures were finalisers killed by a caller's timeout, each leaving a
  marker that blocked every later command; a detached child cannot be killed that way.
- `backups/guard-run.json` — structured run state (stages with start/finish times,
  per-profile state, blockers, exit code) written atomically at every transition.
- `env::profiles(home)` — enumerates the profiles that actually exist, instead of
  assuming the local fleet.

### Fixed

- **`verify` could never fail.** It printed `VERIFY: PASS` and exited 0 on a home
  with no `hermes-agent`, an unreadable HEAD and no venv — and counted git's
  stderr as "1 local modification re-applied". A verifier that cannot fail is
  worse than none: it certifies a broken install. Every check that cannot be
  performed is now a failure and `run()` returns `Err`, so the process exits 1.
- **stdout and stderr were merged**, so a failed `git` invocation's error text was
  parsed as data. `run_checked` keeps them separate and treats a non-zero exit as
  an error.
- **A dropped patch was reported as "nothing was pinned".** A clean tree with no
  stash entries passed the old check even when the pre-update backup recorded a
  working-tree patch. `verify` now reads the file list from the newest
  `pre-update-*/working-tree.patch` and fails when a pinned file is no longer
  modified.
- **The profile fleet was hardcoded to the local seven.** On the droplet
  (hoodbot/sentinel/solana/storemoat) the guard backed up, exported and verified
  the wrong set, or nothing at all, while reporting success.
- **The hermes CLI was looked up only inside the venv.** System installs
  (`/usr/local/bin/hermes`) and the droplet (`~/.local/bin/hermes`, absent from a
  non-interactive PATH) could not be found. `resolve_hermes` now tries the venv,
  the well-known locations, then PATH, and returns `None` rather than a path that
  cannot exist.

### Changed

- 19 tests (was 11); the fail-closed contract, the patch-file parser and the state
  round-trip are covered.

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

[Unreleased]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/MCPVOT/hermes-update-guard/releases/tag/v0.1.0
