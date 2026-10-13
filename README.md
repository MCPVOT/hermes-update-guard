# hermes-update-guard

Guardian orchestrator for **safe Hermes Agent updates** — a single compiled binary
(Windows, Linux, macOS) that wraps `hermes update` with everything the PS1 scripts
did plus a hard preflight gate, WAL-safe backups, and a provider-surface tripwire that
refuses silent model/config drift.

```
preflight → export → backup (WAL-safe, verified) → hermes update --yes → verify
```

Built because real update failures have happened: a node-deps build failure, a desktop
completion that died mid-run (exit 143) leaving a stale `running` receipt, and a
"perpetually dirty" `models_catalog_static.py` that the updater silently stashes.
This tool makes each of those conditions *visible and gated before* you update.

## Why a Rust tool?

- **One static binary** — no PowerShell execution-policy surprises, no BOM/ANSI
  parsing traps, no Python venv dependency. It shells out ONLY to official Hermes
  commands (`hermes update --yes`, `hermes update --check`, git) and to the OS.
- **Real WAL-safe backups** — SQLite online-backup API (same mechanism the PS1
  wrapper used) implemented natively via rusqlite, with `integrity_check` + message
  counts + a **completeness gate** that refuses to proceed if any profile's DB
  fails to back up.
- **Cross-platform by design** — Windows, Linux, macOS. Static CRT on Windows,
  no dynamic deps on POSIX. Same binary walks `%LOCALAPPDATA%\hermes` or `~/.hermes`.
- **Upstream-friendly** — designed to fold into the Hermes ecosystem later; ships
  with a Hermes skill wrapper (`skills/`) and a normal crates.io layout.

## Install / build

```bash
cargo install --path .            # from this repo
cargo build --release             # or build binary at target/release/
```

Prebuilt binaries: see GitHub Releases (when published) — grab per-OS artifact.

## Commands

| Command | What it does | Key gates |
|---|---|---|
| `preflight` | Read-only state check: version, git HEAD/behind/dirty/stashes, venv holders, stale locks, desktop app running, provider drift, gateway state | none (read-only) |
| `export` | Snapshot each profile's provider surface → `backups/provider-surface/<stamp>/` and advance `last-known` | none (read-only on live configs) |
| `diff` | Compare live surface vs **golden** (or `--baseline last-known`/path) | **exit 1 on any drift** |
| `backup` | WAL-safe SQLite backup of every profile's `state.db` + config/env/auth/gateway/content dirs → `backups/pre-update-<stamp>/`, verifies integrity + message counts | **refuses on incomplete backup** |
| `update` | Full guarded update: preflight gate → export → backup → `hermes update --yes` → verify | refuses while venv held or desktop app running (add `--close-desktop`) |
| `verify` | Post-update: HEAD advanced, DB integrity, drift-vs-export, gateway pid/state | reports FAIL on any bad DB |
| `vibe` | Print the MCPVOT identity banner | none |

### Example flow

```bash
# 1. Peek before anything
hermes-update-guard preflight

# 2. Pin the known-good surface (operator's judgement call)
hermes-update-guard export

# 3. Prove no drift just before update
hermes-update-guard diff --baseline last-known

# 4. Take the WAL-safe snapshot
hermes-update-guard backup

# 5. Update, guarded
hermes-update-guard update --close-desktop

# 6. Confirm afterwards
hermes-update-guard verify
```

Exit codes: `0` ok, `1` drift/backup/verify failure, `2+` hard error (bad home, missing git, command failure).

## Provider-surface tripwire

`export`/`diff` protect exactly the config the update must never silently change —
ported verbatim from `hermes-provider-drift.ps1`:

`model.provider`, `model.default`, `model.base_url`, `model.api_key`,
`auxiliary`, `fallback_providers`, `fallback_model`, `delegation`,
`providers`, `moa`, `display.language`, `stt.language`

Secrets are **masked** (`sk-…`/`nvapi…` truncated) before anything is written;
the manifest records SHA-256 of each snapshot. The tool never writes to live
config or `.env` — only to `backups/`.

## Layout / development

```
src/
  main.rs      CLI (clap)
  env.rs       home/profile discovery, os probes, command runner
  drift.rs     surface extract/export/diff (PS1 semantics)
  backup.rs    WAL-safe DB backup + completeness gate
  guard.rs     full update orchestration
  verify.rs    post-update verification
  preflight.rs read-only state check
skills/        hermes skill wrapper (for Hermes agents)
.github/       CI (matrix) + release workflows
```

```bash
cargo test                       # unit tests (masking, yaml extract, path lookups)
cargo clippy -- -D warnings      # lint gate (CI)
cargo fmt --check                # format gate (CI)
cargo build --release            # static-CRT standalone exe on Windows
```

## Publishing

From your own GitHub account, when ready:

1. Replace `YOUR_USERNAME` in `Cargo.toml` (repository/homepage).
2. `git tag v0.x.y` and push — the release workflow builds
   Windows/Linux/macOS binaries and drafts a GitHub Release.
3. Optional crates.io: `cargo publish` (requires a crates.io token).

Contributor conventions follow the Hermes repo (`docs/`: commit types `fix:`,
`feat:`, `refactor:`, `docs:`, `chore:`; tools return structured output; never
print secrets).

## Security

Secrets policy: see `SECURITY.md`. This tool only *reads* `.env`/`auth.json` to
*back them up byte-for-byte into `backups/`*, never prints them, never modifies
live config. Report issues privately per `SECURITY.md`.

## License

MIT — see `LICENSE`.