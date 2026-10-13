# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/MCPVOT/hermes-update-guard/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/MCPVOT/hermes-update-guard/releases/tag/v0.1.0
