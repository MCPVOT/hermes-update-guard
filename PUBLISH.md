# hermes-update-guard — handoff note for publishing

**What:** a single Rust binary that guards `hermes update`. One command runs the
whole safe-update pipeline: `preflight → export → backup → hermes update --yes → verify`.

**Why it was created:** real update failures happened on this machine — a
half-finished update (desktop-build step died with exit 143) left a stale
`running` receipt and a stale `.hermes-update-in-progress.lock`; a locally
patched core file (`hermes_cli/models_catalog_static.py`) gets silently
stashed by the updater; and model/provider config can drift without anyone
noticing. The old PowerShell wrappers (`update-hermes.ps1`,
`hermes-provider-drift.ps1`) solved this but are Windows-only, fragile to
run from agents (execution policy, BOM/ANSI traps), and live outside version
control. This tool is their cross-platform, compiled, version-controlled
successor — designed to be published and later offered to the Hermes
ecosystem.

## How it works (subcommands)

| Command | Does | Gate |
|---|---|---|
| `preflight` | read-only: version, git HEAD/behind/dirty/stashes, venv holders, stale locks, desktop app, drift summary, gateway | — |
| `export` | snapshot each profile's provider surface (12 protected keys, secrets masked, sha256 manifest), advance `last-known` | — |
| `diff` | live vs golden (or `--baseline last-known`/path) | **exit 1 on drift** |
| `backup` | WAL-safe SQLite online-backup of every profile `state.db` + config/.env/auth/content dirs; integrity + message counts | **refuses if incomplete** |
| `update` | preflight gate → export → backup → `hermes update --yes` → verify | refuses if venv held / desktop running (`--close-desktop`) |
| `verify` | HEAD advanced, DB integrity all profiles, drift vs pre-update export, gateway | reports FAIL on bad DB |
| `vibe` | MCPVOT banner | — |

Key design facts: home = `HERMES_HOME` only when it's a root (has
`hermes-agent/` or `profiles/`), else `%LOCALAPPDATA%\hermes` / `~/.hermes`,
or `--home`; static CRT on Windows (standalone exe), POSIX uses `pgrep`/`pkill`;
secrets are never printed; `.env`/`auth.json` only copied into `backups/`.

## Verified state at handoff (2026-10-13)

- `v0.1.1` — 4 commits, tag `v0.1.1` on the final commit, MCPVOT identity, tree clean
- Gates green: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, **7/7 tests**, release build OK
- Exercised against the live machine: preflight/export/diff clean vs last-known, real WAL-safe backup of all 7 profile DBs (integrity=ok), release binary runs standalone
- Docs: README (commands, exit codes, publishing), CHANGELOG 0.1.1, SECURITY.md, CI + draft-release workflows (Windows/Linux/macOS), Hermes skill wrapper in `skills/`

## Publish checklist (for the publishing agent)

1. `cd C:/Users/Organix/Dev/hermes-update-guard` — verify gates yourself:
   `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
2. Replace **`YOUR_USERNAME`** in `Cargo.toml` (repository + homepage) with the
   real GitHub username; update README install/links if needed; commit (`chore:`).
3. Confirm `gh auth status` is logged in to that account.
4. `gh repo create hermes-update-guard --public --description "Guardian orchestrator for safe Hermes Agent updates: preflight, WAL-safe backup, provider-drift tripwire, update, post-verify" --source . --push`
5. Push the tag: `git push origin master --tags` — the release workflow builds
   the three OS binaries and leaves a **draft** GitHub Release.
6. Review the draft release (artifacts present), then publish it in the UI.
7. Report the repo URL + release URL back. Do NOT `cargo publish` to crates.io
   without explicit approval.

## Invariants (do not break)

- No secrets anywhere: never print `.env`/`auth.json` contents; never commit them.
- Git identity for new commits: `MCPVOT <mcpvot@users.noreply.github.com>`.
- The release must stay a draft until reviewed. Nothing publishes without the owner.
- MIT license text stays as-is; the MCPVOT vibe banner (`vibe` subcommand) stays.