# Live `watch` view: architecture, theme, and how to smoke-test a TUI

How the guarded updater exposes progress while it runs, and the parts that generalise
to any long-running command that needs a live view.

## The contract: one state file, one direction

```
guard  --writes-->  backups/guard-run.json  --reads-->  watch (TUI)
        --writes-->  backups/guard-update.log --tails-->  watch (TUI)
```

**The viewer is read-only, and that is a safety property, not a style choice.** A
viewer that can write can strand an install: a crash, a stray keypress, or a
half-written file must never be able to influence a backup or an update. Keep the
state file the *only* channel between the two, so the viewer can be killed at any
moment with no consequence.

Write the state **atomically** — serialize to `*.json.tmp`, then rename. A reader that
poll-writes on a tick will otherwise eventually read a truncated file and either panic
or (worse) render zeros as if they were measurements.

Treat a missing or unparseable state file as a normal state (`waiting for
backups/guard-run.json`), never an error. The viewer starts before the run does, and
runs after it ends.

## What makes the view worth having

- **Per-unit progress through the silent phases.** A WAL-safe backup of seven SQLite
databases is minutes of complete silence. One row per profile, updated as each
completes, converts a dead cursor into evidence of progress. Get this by taking a
progress callback into the long phase (`backup::run_with(home, dry_run, &mut |profile,
ok| ...)`) rather than printing after it finishes.
- **A stage rail, not a spinner.** `preflight > export > backup > update > verify` with
each stage coloured done/active/waiting tells you *where* it is, which is the question
you actually have.
- **A blockers band.** Lift only what changes a decision to the top: venv held, real
drift, stale marker, unfinished receipts. Everything else stays in the log tail.
- **Severity-coloured log tail.** Grep-scale information without a second terminal.

## Detaching: the update must not die to its caller

Run the real work in its own process group and return immediately. Redirect the
child's stdout+stderr to a log file, and let the TUI tail that file.

```rust
let mut cmd = Command::new(std::env::current_exe()?);
cmd.arg("update").arg("--home").arg(home);
cmd.stdin(Stdio::null());
cmd.stdout(Stdio::from(file.try_clone()?)).stderr(Stdio::from(file));
#[cfg(windows)] { use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0000_0008 | 0x0000_0200); } // DETACHED_PROCESS | NEW_PROCESS_GROUP
```

The parent spawns the child **without** the detached flag, so the child runs the
normal path and owns the state file. Two historical failures were finalisers killed by
a caller's timeout, each leaving a marker that blocked every later run.

## Theme (boss palette, ported to the terminal)

The locked palette is colour values, not CSS. Map hex to `Color::Rgb`:

| Role | Hex | RGB | Use |
|---|---|---|---|
| background | `#050807` | 5,8,7 | header/module background |
| surface | `#0b0f0d` | 11,15,13 | panel fill |
| line | `#143229` | 20,50,41 | borders |
| cyan | `#00e5ff` | 0,229,255 | active / next |
| green | `#00ff9c` | 0,255,156 | done |
| amber | `#f0b25a` | 240,178,90 | waiting / warning — emphasis only |
| text / muted / dim | `#e7f5ee` · `#7fb8a0` · `#3f5a50` | 231,245,238 · 127,184,160 · 63,90,80 | body / labels / inactive |

No pink, no purple, no matrix rain. Status semantics stay fixed: green = done,
amber = waiting, cyan = next.

## Smoke-testing a TUI without seeing it

A terminal UI has no DOM to query, so drive it through a pty and assert on bytes:

1. Start it in a **background pty** session.
2. Poll until the frame appears; assert the output contains strings you expect
   (a footer, a stage name) and contains no `panic`.
3. Send a quit key with `process_manage action=write data="q"`.
4. `action=wait` and require **exit 0**.

Use `write` (raw bytes), **not** `submit`: submit appends Enter, and on a Windows PTY
Enter is a carriage return, so a `\n` never terminates the line and the keypress is
silently ignored. This works for any single-character key handler.

Also render the same state through a non-TUI path (`watch --no-tui`) and assert on the
text: it proves the state file, the parser and the formatting independently of terminal
handling, and it is the only variant a script or CI can assert on.

## Pitfalls

- **Raw mode / alternate screen leaked on error.** Wrap the event loop and always run
  `disable_raw_mode()` + `LeaveAlternateScreen` + `show_cursor()` before returning, or a
  panic leaves the operator's terminal unusable.
- **No minimum-size guard.** A narrow terminal makes `Layout` constraints resolve to
  zero and the frame renders as noise. Check `area.height`/`area.width` first and show
  a plain "terminal too small" line.
- **Assuming a fixed widget API across ratatui versions.** 0.29 uses `f.area()`, not the
  older `f.size()`. Compile after bumping rather than porting from memory.
- **Matching a status word by substring in the wrong stream.** Colour the log by
  severity only after normalising case once; a line containing `error` inside a path or
  a model name will otherwise paint red for no reason.
- **Rendering a running timer from the wrong clock.** Compute elapsed from the run's own
  start stamp and freeze it at `finished_at`, or a finished run keeps counting.
