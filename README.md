# hermes-update-guard

[![CI](https://github.com/MCPVOT/hermes-update-guard/actions/workflows/ci.yml/badge.svg)](https://github.com/MCPVOT/hermes-update-guard/actions/workflows/ci.yml)
[![Release](https://github.com/MCPVOT/hermes-update-guard/actions/workflows/release.yml/badge.svg)](https://github.com/MCPVOT/hermes-update-guard/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org)

**Guardian orchestrator for safe Hermes Agent updates** — one compiled binary
(Windows, Linux, macOS) that wraps `hermes update` with a hard preflight gate,
WAL-safe backups, and a provider-surface tripwire that refuses silent
model/config drift.

```
preflight → export → backup (WAL-safe, verified) → hermes update --yes → verify
```

Hermes updates can fail in ways that stay invisible until something breaks: a
build step that dies mid-run and leaves a stale `running` receipt, a lock file
that never clears, a locally patched core file the updater silently stashes, or
provider/model config that drifts without anyone noticing.
`hermes-update-guard` makes each of those conditions **visible and gated before**
the update runs — then verifies the result afterwards.

## Contents

- [Why a Rust tool?](#why-a-rust-tool)
- [Install](#install)
- [Commands](#commands)
- [Quick start](#quick-start)
- [Provider-surface tripwire](#provider-surface-tripwire)
- [Exit codes](#exit-codes)
- [Safety guarantees](#safety-guarantees)
- [Requirements](#requirements)
- [Development](#development)
- [Uninstall](#uninstall)
- [Use it from Hermes (skill)](#use-it-from-hermes-skill)
- [Contributing](#contributing)
- [License](#license)

## Why a Rust tool?

- **One static binary** — no PowerShell execution-policy surprises, no BOM/ANSI
  parsing traps, no Python venv to keep alive. It shells out only to the official
  Hermes CLI (`hermes update --yes`, `hermes update --check`) and to `git`.
- **Real WAL-safe backups** — SQLite's online-backup API via `rusqlite`, with an
  `integrity_check` plus message counts and a **completeness gate** that refuses
  to proceed if any profile's database fails to back up.
- **Cross-platform by design** — static CRT on Windows, no dynamic dependencies
  on POSIX. The same binary finds the Hermes home whether it is
  `%LOCALAPPDATA%\hermes`, `~/.hermes`, or `$HERMES_HOME`.
- **Upstream-friendly** — it changes no Hermes core code; it orchestrates the
  official update path and ships a [Hermes skill wrapper](#use-it-from-hermes-skill)
  so an agent can drive it safely.

## Install

**Prebuilt binaries.** Download the artifact for your OS from the
[Releases page](https://github.com/MCPVOT/hermes-update-guard/releases):

| OS | Artifact |
|---|---|
| Windows (x86_64) | `hermes-update-guard-<version>-x86_64-pc-windows-msvc.exe` |
| Linux (x86_64) | `hermes-update-guard-<version>-x86_64-unknown-linux-gnu` |
| macOS (x86_64) | `hermes-update-guard-<version>-x86_64-apple-darwin` |

```bash
# Linux / macOS — fetch, mark executable, put it on PATH
VER=0.3.0
curl -fsSL -o hermes-update-guard \
  "https://github.com/MCPVOT/hermes-update-guard/releases/download/v${VER}/hermes-update-guard-${VER}-x86_64-unknown-linux-gnu"
chmod +x hermes-update-guard
sudo mv hermes-update-guard /usr/local/bin/          # or ~/.local/bin
```

**From source** (needs a stable Rust toolchain):

```bash
cargo install --path .        # installs to ~/.cargo/bin
cargo build --release         # or run target/release/hermes-update-guard
```

## Commands

| Command | What it does | Gate |
|---|---|---|
| `preflight` | Read-only state check: version, git HEAD/behind/dirty/stashes, venv holders, **unfinished update receipts**, stale locks, desktop app running, provider drift summary, gateway state | none (read-only) |
| `export` | Snapshot each profile's provider surface into `backups/provider-surface/<stamp>/`, advance `last-known`, refresh the aggregate `golden.json` | none (read-only on live config) |
| `diff` | Compare the live surface against **golden** (or `--baseline last-known` / a path) | **exit 1 on any drift or unreadable baseline** |
| `backup` | WAL-safe SQLite backup of every profile's `state.db` plus config/env/auth/gateway/content dirs into `backups/pre-update-<stamp>/`; verifies integrity + message counts | **refuses on incomplete backup** |
| `update` | Full guarded update: preflight gate → export → backup → `hermes update --yes` → verify. Add `--detached` to run it in its own process group so a closed terminal or a caller timeout cannot kill the build | refuses while a venv is held (or its state is unknown) or the desktop app is running (add `--close-desktop`) |
| `verify` | Post-update: HEAD advanced, DB integrity, **local-patch restoration**, drift-vs-export, gateway pid/state | **exit 1 on any failed or unperformable check** |
| `watch` | Live, read-only view of a run: stage rail, per-profile backup matrix, blockers, log tail (`--no-tui` for a one-shot snapshot) | none (read-only) |
| `vibe` | Print the MCPVOT identity banner | none |

## Quick start

```bash
# 1. Peek before anything
hermes-update-guard preflight

# 2. Pin the known-good provider surface (an operator judgement call)
hermes-update-guard export

# 3. Prove no drift just before updating
hermes-update-guard diff --baseline last-known

# 4. Take the WAL-safe snapshot
hermes-update-guard backup

# 5. Update, guarded
hermes-update-guard update --close-desktop

# 6. Confirm afterwards
hermes-update-guard verify
```

`update` runs the whole pipeline in one shot, so steps 2–6 can be collapsed into
a single `hermes-update-guard update --close-desktop`. The individual subcommands
exist so you can inspect or stop at any stage.

## Provider-surface tripwire

`export` / `diff` protect exactly the config an update must never silently
change — the same twelve keys the original drift script guarded:

`model.provider`, `model.default`, `model.base_url`, `model.api_key`,
`auxiliary`, `fallback_providers`, `fallback_model`, `delegation`,
`providers`, `moa`, `display.language`, `stt.language`

Secrets are **masked** (`sk-…` / `nvapi…` truncated) before anything is written,
and the manifest records a SHA-256 of each snapshot. The tool never writes to
live config or `.env` — only to `backups/`.

### Baseline shapes

A baseline is any of three shapes found in the wild, and each profile is compared
against its **own** slice of it:

| Baseline | Shape | Read as |
|---|---|---|
| `golden.json` (default) | one JSON object **per profile** | `base[<profile>]` |
| `last-known/` | a directory of one flat file per profile | `last-known/<profile>.json` |
| explicit `--baseline <path>` | one flat surface | the file itself |

Two rules make the tripwire trustworthy rather than merely loud: files are read
BOM/CRLF-tolerantly (the PowerShell exporter writes a UTF-8 BOM), and a baseline
that exists but cannot be parsed is reported as `baseline UNREADABLE` and
**fails** the check. An aggregate that does not list a profile is "no baseline
yet", never a drift storm.

### Interrupted-update receipts

`preflight` also reads `logs/update_receipts/*.json`. A receipt left in `running`
or `interrupted` state with `finished_at: null` means the previous update died
mid-flight — and that hermes will keep retrying the work it owed on every launch
until a real `hermes update` completes it. The pid, start time and target version
are printed so you can tell a genuine in-flight update from an abandoned one.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | success |
| `1` | drift found, backup incomplete, verify failure, or any runtime error |
| `2` | CLI usage error (bad flags/arguments) |

## Safety guarantees

- **No secret leakage** — the tool never prints `.env` / `auth.json` contents.
  They are copied byte-for-byte into `backups/` (which inherits the Hermes home's
  own access controls) and never modified in place.
- **No live writes** — every subcommand is read-only against live state except
  through the official `hermes update` CLI.
- **No phone-home** — the binary makes no network calls of its own.

Full policy and how to report a vulnerability: [SECURITY.md](SECURITY.md).

## Requirements

- A working `hermes` CLI on `PATH` — this tool orchestrates it, it does not
  replace it.
- Windows 10/11, Linux, or macOS on x86_64.
- A stable Rust toolchain **only** if you are building from source.

## Development

```
src/
  main.rs      CLI (clap)
  env.rs       home/profile discovery, OS probes, command runner
  drift.rs     provider surface: extract / export / diff
  backup.rs    WAL-safe DB backup + completeness gate
  guard.rs     full update orchestration
  verify.rs    post-update verification
  preflight.rs read-only state check
  vibe.rs      MCPVOT identity banner
skills/        Hermes skill wrapper (for Hermes agents)
.github/       CI matrix + release workflows
```

```bash
cargo fmt --check                            # format gate (CI)
cargo clippy --all-targets -- -D warnings    # lint gate (CI)
cargo test                                   # unit tests (masking, YAML extract, path lookups)
cargo build --release                        # static-CRT standalone exe on Windows
```

## Uninstall

Delete the binary and (optionally) the backups it wrote:

```bash
rm ~/.local/bin/hermes-update-guard     # or wherever you installed it
rm -rf "$HERMES_HOME/backups"           # only if you no longer want the snapshots
```

## Use it from Hermes (skill)

A Hermes skill wrapper ships in
[`skills/hermes-update-guard/`](skills/hermes-update-guard/SKILL.md). Copy it
into `~/.hermes/skills/`, or add this repo as a tap, and Hermes loads it
automatically whenever a task mentions `hermes update`:

```bash
# copy the wrapper in
mkdir -p ~/.hermes/skills && cp -r skills/hermes-update-guard ~/.hermes/skills/

# ...or install straight from this repo
hermes skills install github:MCPVOT/hermes-update-guard/skills/hermes-update-guard
```

## Contributing

Issues and pull requests are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT — see [LICENSE](LICENSE).
