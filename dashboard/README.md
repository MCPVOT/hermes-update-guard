# hermes-guard-dashboard

A pop-up window that shows a guarded update while it runs — **in the Hermes
desktop's own palette**, so it looks like a panel of Hermes rather than a second
product.

## Why it exists

`hermes update` closes the Hermes desktop app. The preflight gate insists on it,
because a running desktop holds the venv and a half-live app during a dependency
build is how installs get stranded. That leaves two problems:

1. the `watch` TUI needs a terminal, and
2. the operator has no window to look at while the build runs.

This is that window. It is a separate process with its own WebView2 instance, so
the desktop closing does not close it, and it can be pinned on top.

## Read-only by construction

The window has exactly two commands:

| command | effect |
|---|---|
| `snapshot()` | reads `backups/guard-run.json` + tails `guard-update.log` |
| `set_always_on_top(on)` | pins the window |

It cannot start, stop or alter an update. The same rule the TUI follows: a viewer
that can break an update is a liability, and a viewer crash must never strand an
install.

## Palette

Lifted from `apps/desktop/src/styles.css` in the Hermes install, dark skin:

| token | value | used for |
|---|---|---|
| `--bg` | `#0d0d0e` | window chrome |
| `--bg-sidebar` | `#0a0a0b` | header / footer bars |
| `--card` | `#161618` | panels |
| `--accent` | `#0053fd` | active stage, running verdict |
| `--ok` | `#55a583` | done |
| `--fail` | `#e75e78` | failed check |
| `--warn` | `#d8a657` | blockers |
| `--text` | `rgba(255,255,255,.94 / .74 / .54)` | primary / secondary / tertiary |
| `--border` | `rgba(255,255,255,.09)` | strokes |
| `--radius` | `12px` | panels (Hermes uses `0.75rem`) |

To re-skin, change only the `:root` block in `ui/style.css`. Nothing else in the
app hardcodes a colour.

## Build and run

WebView2 is required on Windows (ships with Windows 11 and current Edge). No Node
toolchain, no bundler: the UI is plain HTML/CSS/JS emitted straight from `ui/`.

```bash
cd dashboard
cargo build --release
./target/release/hermes-guard-dashboard.exe            # uses the default Hermes home
./target/release/hermes-guard-dashboard.exe --home C:/Users/you/AppData/Local/hermes
```

The window polls `snapshot()` every 600 ms. It shows an honest empty state
("waiting for a run") when no `guard-run.json` exists — it never invents data.

## Deliberately not in this crate

- **No installer / bundling.** `bundle.active = false`; this ships as a bare exe.
  Add the Tauri CLI and an icon set only if it ever needs an MSI.
- **No workspace membership.** The CLI must build with `cargo build` and nothing
  else, on Linux CI included. A GUI crate would drag `webkit2gtk` into that
  matrix, so this crate is excluded from packaging and built on demand.

## Files

```
dashboard/
├── Cargo.toml          tauri 2, serde, chrono
├── build.rs            tauri_build::build()
├── tauri.conf.json     window 1120×740, dark, withGlobalTauri
├── src/main.rs         snapshot() + set_always_on_top()
└── ui/
    ├── index.html      structure
    ├── style.css       Hermes palette, panels, stage rail
    └── app.js          poll loop + render
```

## State contract

It reads the same file the TUI reads, so both views can be open at once:

```json
{
  "run_id": "20261021-204000",
  "version": "0.3.4",
  "started_at": "2026-10-21T20:40:00",
  "finished_at": null,
  "exit_code": null,
  "stages":  [{ "name": "preflight", "state": "done" }],
  "profiles":[{ "name": "ofcrm", "state": "done", "detail": "state.db verified" }],
  "blockers":["3 unfinished update receipt(s)"],
  "log_path": ".../backups/guard-update.log"
}
```

`state` values are `waiting | active | done | failed`.
