# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.5] - 2026-10-21

Fixes from a five-agent audit of the repo (workflows, README, skill, git history,
log-signal quality). Every finding below was reproduced before it was changed.

### Security

- **Nested credentials were exported in clear.** `mask()` tested only the
  top-level string, so `auxiliary.<role>.api_key`, `providers.<name>.api_key` and
  `delegation.*.token` were written to `last-known/*.json` and echoed to the
  terminal. It is now recursive, masking on two rules: a value that *looks* like a
  key (`sk-`, `nvapi`, `ghp_`, `github_pat_`, `xox`, `AIza`, `AKIA`, `glpat-`)
  wherever it appears, and a key that *names* a credential (`api_key`, `token`,
  `secret`, `password`) even when its value carries no known prefix. `token_type`
  and `max_tokens` stay readable.
- **A personal path was committed** in `PUBLISH.md` (`C:/Users/<name>/...`).
  Replaced with `<your checkout>`.
- **Short secrets were nearly transparent.** `mask_str` kept the first 8 and last 4
  characters of anything over 12, so a 13-character credential was 12/13 visible
  and a prefixed token was close to recoverable. Below 32 characters a value is now
  masked whole; above that, 4 + 4.
- **Two nested shapes still exported in clear** (found by re-testing the first fix
  against real provider shapes): `Authorization: Bearer <token>` — the key name says
  nothing and the value starts with a scheme, so neither rule fired — and a bare
  `key:` slot. `authorization`/`bearer`/`cookie` and a bare `key` are credential
  slots now, a `Bearer`/`Basic` value counts as secret on sight, and pointers such
  as `api_key_env` / `*_name` / `*_url` stay readable because they name a
  credential rather than being one.

  Verified end to end on a synthetic home containing all three shapes: the exported
  `p1.json` contains no full token (`FULL TOKEN PRESENT IN FILE: False`).

### Fixed - log signals

- **No grouping at all.** 93.5% of live signatures began with a literal
  timestamp, so every occurrence was unique and `count` was always 1 (23,805
  signatures, 99% singletons). The timestamp is stripped before normalising.
- **One traceback produced three signatures** (the `raise` frame, an `EXC` line and
  the exception line). A traceback is now exactly one event, identified by its
  final exception line.
- **Rotated logs were invisible** - only `extension() == "log"` was read, hiding
  ~35 MB of `*.log.1/.2/.3` and making a rotated failure unable to register as
  new. Rotations are read and labelled distinctly (`agent.log.2`), so an upgrade
  does not relabel existing signals.

Measured on the live fleet: 23,787 -> **20,888** distinct signatures, with the top
entries now carrying real counts (`x5472`) instead of singletons.

### Fixed - release workflow

- **The release path had no test gate.** `release.yml` now has a `gate` job the
  binaries `needs`: `cargo fmt --check`, `clippy -D warnings`, `cargo test
  --all-targets`, and a refusal to release a tag whose commit is not on
  `origin/master`. Previously a tag on any branch released untested code.

### Fixed - docs that contradicted the code

- README install snippet pinned `VER=0.3.0`, a binary without five documented
  commands.
- README said `export` refreshes `golden.json`; it deliberately does not - only
  `bless` moves the approved reference.
- README said "twelve keys"; `SURFACE_KEYS` has thirteen.

### Notes

- 47 tests (was 45).

### Added

- **`dashboard/` — a pop-up Tauri window** that shows a run while Hermes itself is
  closed by the update. Same palette as the Hermes desktop (chrome `#0d0d0e`,
  card `#161618`, accent `#0053fd`, text at 94/74/54% white). It has exactly two
  commands — `snapshot()` and `set_always_on_top()` — so it is read-only by
  construction, like the TUI. No Node toolchain: the UI is plain HTML/CSS/JS
  served straight from `ui/`, and the crate is excluded from packaging so the
  CLI's 3-OS CI never has to pull in `webkit2gtk`.
- README: a mermaid diagram of how a run is gated, plus a live-view section
  covering `watch`, `watch --no-tui` and the dashboard.

### Fixed (dashboard, found by rendering it)

- The stage rail lived in the header row and clipped **VERIFY** off the right edge
  once the run id was long; it now has its own row and wraps.
- Long log lines were cut off; the pane wraps anywhere now.

## [0.3.4] - 2026-10-21

Completes and hardens the in-progress `logsignals` / `mcpcheck` work: the session
that wrote it ended mid-implementation, so none of it had been built, tested,
committed or verified against a live install.

### Fixed — `mcp` readiness (five defects, two of them false alarms)

- **Every enabled non-OAuth server was reported broken.** `is_broken()` judged by
  token state alone, so an `auth: none` / stdio server — which never has a token
  file — was flagged on a healthy config. Live proof: `default/onlyfans_docs
  auth=none` was reported as "cannot authenticate headlessly".
- **Per-profile token stores were never read.** Tokens live under
  `profiles/<p>/mcp-tokens` before the root store; checking only the root reported
  `ofcrm/onlyfans` as "no OAuth client registered" while it had a client and token.
  It now resolves through the profile store and reports the truth: `token=expired`.
- **An unparseable expiry read as valid.** A line-scanner missed single-line or
  malformed token files and defaulted to `ok`; the file is now parsed as JSON and
  anything unreadable is `unknown` (fail closed).
- **Any refresh lock was called "stale".** Staleness is now an age test (10 min).
- Removed a dead `if client.exists() { "missing" } else { "missing" }` branch.

### Fixed — log signals (four defects, two of them crashes)

- **Timestamps never parsed.** `parse_ts` required `:` at index 10, where a real
  timestamp has a space or `T`, so every signature reported an empty `last_seen`.
- **Two panic paths.** A char-built String was byte-sliced (`s[4..5]`) and
  `String::truncate(140)` was used — both panic on a non-char boundary, i.e. any
  log line containing non-ASCII text crashed the guard.
- **`ERROR:` / `[ERROR]` / `level=ERROR` were missed**, because the check required
  the literal `" ERROR "` with spaces on both sides.

### Fixed — wiring

- **`health --logs` did nothing.** The flag was parsed and never passed through,
  so `run_with(.., true)` was unreachable.
- **And when it did run it printed nothing**, because context health exited before
  the log report. The report now prints first, then the exit code is applied.

Measured on the live install: `onlyfans_docs` no longer flagged; `ofcrm/onlyfans`
reports a real `exp=2026-10-21` expiry; `health --logs` surfaces 204 ×
`OAuthNonInteractiveError` in ofcrm.

### Notes

- 45 tests (was 36).

## [0.3.3] - 2026-10-21

### Added

- `retire <profile>` (and `--undo`): records that a profile is **gone on purpose**
  in `backups/profile-retired.json`, with the date and the last backup that saw it.
  `self-check` stops flagging it as missing.

  This is the counterpart to the vanished-profile check added in 0.3.2. An
  intentional deletion and an accidental one look identical on disk, and a checker
  that keeps reporting a decision the operator already made is a checker people
  learn to ignore. Like `bless`, the reference moves only on an explicit operator
  action, never as a side effect of a check.

  It refuses to retire a profile that still exists on disk — hiding a live profile
  from the fleet checks is the exact failure this tool exists to prevent.

### Notes

- 36 tests (was 34).

## [0.3.2] - 2026-10-21

### Fixed

- **CI was red on Linux and macOS.** Two independent causes, both from 0.3.1:
  `cargo fmt --all -- --check` failed across 25 positions, and the Windows-only
  helpers `csv_rows` / `parse_tasklist_csv` were unconditionally compiled, so
  `clippy -D warnings` reported them as `dead_code` off Windows. Formatted, and
  both parsers are now `#[cfg_attr(not(windows), allow(dead_code))]` — still
  compiled everywhere so their tests run on any host.
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

[Unreleased]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.3.5...HEAD
[0.3.5]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.3.4...v0.3.5
[0.3.4]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.3.3...v0.3.4
[0.3.3]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/MCPVOT/hermes-update-guard/releases/tag/v0.1.0
