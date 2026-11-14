---
name: hermes-update-guard
description: Use when running, verifying, repairing or auditing a `hermes update` — the guarded pipeline (preflight, WAL-safe backup, drift tripwire, post-verify) via the hermes-update-guard binary.
version: 0.7.2
author: MCPVOT
license: MIT
platforms: [macos, linux, windows]
metadata:
  hermes:
    tags: [hermes, update, backup, drift, guardian, devops]
    homepage: https://github.com/MCPVOT/hermes-update-guard
    related_skills: [hermes-agent]
    requires_toolsets: [terminal]
---

# hermes-update-guard

Guardian orchestrator for safe `hermes update` runs. It wraps the official update
CLI with a preflight gate, WAL-safe SQLite backups, and a provider-surface
tripwire — the whole scripted update ritual in one static binary.

## When to Use

- Before ANY `hermes update`: run the guarded flow below instead of updating raw.
- After an update: prove nothing drifted (version, DB integrity, provider surface).
- When a previous update failed and left a stale lock, a stale `running` receipt,
  or a silently stashed core file.

## Quick Reference

| Task | Command |
|---|---|
| Peek before updating | `hermes-update-guard preflight` |
| Fleet transcript health (read-only) | `hermes-update-guard health` |
| Pin the known-good surface | `hermes-update-guard export` (advances `last-known`, never `golden.json`) |
| Move the APPROVED baseline | `hermes-update-guard bless` (the only thing that re-blesses `golden.json`) |
| Prove no drift | `hermes-update-guard diff` (measures the live surface vs `golden.json`) |
| Clear stale markers/receipts | `hermes-update-guard quarantine [--apply] [--receipts]` |
| WAL-safe backup | `hermes-update-guard backup` |
| Guarded update (all-in-one) | `hermes-update-guard update --close-desktop` |
| Guarded update that must survive a gateway restart or caller timeout | `hermes-update-guard update --detached` |
| Verify after | `hermes-update-guard verify` |
| Snapshot log signatures by hand | `hermes-update-guard snapshot-logs` |
| MCP credential readiness | `hermes-update-guard mcp` |
| Is this binary the one I think? | `hermes-update-guard self-check [--repo <checkout>]` |
| Retire a deliberately-removed profile | `hermes-update-guard retire <profile>` |
| Identity banner | `hermes-update-guard vibe` |

## Procedure

1. **Confirm the binary is available** (see *Binary availability* below).
2. **Run the ritual in order** — adjust `GUARD` to the absolute path if the
   binary is not on `PATH`:

   ```bash
   GUARD=hermes-update-guard

   $GUARD preflight                        # read-only: any surprises?
   $GUARD export                           # pin known-good provider surface
   $GUARD diff --baseline last-known       # prove clean (exit 0)
   $GUARD backup --dry-run                 # prove the target is writable (see below)
   $GUARD backup                           # WAL-safe snapshot of every profile
   $GUARD update --close-desktop           # preflight -> export -> backup -> update -> verify
   $GUARD verify                           # post-update integrity + drift
   ```

3. **Apply the decision rules** (below) at each gate before continuing.

**Launch with `--detached` when the caller lives inside the gateway.** The update pauses and
restarts `hermes-gateway`, which is also the process serving the turn that started it, so a
foreground run dies with that turn and a caller timeout can kill it mid-build. `--detached` forks
the run and returns immediately; poll `$HERMES_HOME/backups/guard-run.json` (stage states,
`exit_code`) and `$HERMES_HOME/backups/guard-update.log` for the outcome instead of holding the
turn open. End the turn promptly either way: the updater waits for in-flight turns before it
pauses the gateway (`Waiting up to 1885s for in-flight turns`), so a long tool call delays the
restart rather than preventing it.

## Binary availability

The expected binary is `hermes-update-guard` on `PATH` (or `target/release/` of a
local build). If it is not installed:

- **Build it from source** — `cargo build --release` in a clone, or `cargo install --path <repo>`.
  Prefer this over any download for a tool that will orchestrate an update on a live host: you can read
  exactly what you are about to run. (A repo with no published releases has no artifact to fetch at all —
  check the releases page before planning around one.)
- Only if a release artifact exists for the target OS: download it, `chmod +x`, place it on `PATH`.

## Prove the backup target before the run

`backup` and `update` both gate on the target: it must be a real mount point unless it is the
control root (`<home>/backups`) or `--allow-local-backup` is passed. An unplugged disk is
REFUSED rather than silently becoming a directory on the root filesystem — good design, but it
stops the run, so prove the target first:

```bash
hermes-update-guard backup --dry-run    # prints target, device, fstype, free, and the plan
findmnt -no TARGET,FSTYPE <target>
```

- **A dirty NTFS volume mounts READ-ONLY and the read-write mount fails outright.** udisks says
  `Error mounting ... wrong fs type, bad option, bad superblock`; the kernel log says
  `ntfs3(sda2): volume is dirty and "force" flag is not set!`. That is Windows fast startup or an
  unclean unmount, not failing hardware: either clear it (`ntfsfix` on the unmounted volume) or
  back up to the control root for this run, and say which you chose.
- The control root always passes and needs no flag. Measure its free space in the dry run: three
  profiles need well under 1 GB, so tens of GB free is never the constraint — honesty about WHERE
  the restore point landed is.
- The dry run prints `[dry-run] plan only - nothing written` plus one line per profile DB, and it
  states `secrets: included (.env, auth.json)`. Decide `--no-secrets` at the real run, not here.

## Running it on a SERVER (no desktop, no operator at the console)

Verified by reading the source before trusting it on a production host — do the same on any new version:

- It executes only `git` (read-only: `rev-parse`, `status`, `stash list`, `diff`), the **official `hermes`
  CLI** (`--version`, `update --check`, `update --list-venv-holders`, `update --yes`), and `pgrep`. No
  network client of its own; writes land only under `<hermes-home>/backups/`; secrets are masked on export.
- The desktop-app probe is per platform since 0.4.1 (`Hermes.exe` on Windows,
  `Hermes.app/Contents/MacOS/Hermes` on macOS, `linux-unpacked/Hermes` on Linux), so it can see a desktop
  app on every host — including a Linux seat, where it used to print `[desktop app] not running` over a
  live process and made `--close-desktop` a no-op that let the rebuild overwrite the packaged app. On a
  headless SERVER there is simply no app to close, so `--close-desktop` is unnecessary there rather than
  dangerous. If a seat still prints `not running` while `pgrep -f 'linux-unpacked/Hermes'` finds the app,
  the probe does not match that build: stop and report it.
- **Check the venv gate first**: `hermes update --list-venv-holders` must print `[]`. A running gateway does
  not by itself hold the venv, but a session that does will make the guard refuse — clear it before
  scheduling rather than debugging the refusal later.
- A server run needs no `--close-desktop`; every other gate (drift, partial backup, doctor) applies unchanged.
- Sensible cadence where the platform runs unattended jobs: a **daily read-only `preflight`/`diff` line into
  whatever the host already reports**, and the **actual update weekly at a low-traffic hour** — the guard's
  refuse-on-drift and refuse-on-partial-backup gates are the protection, so the schedule only has to be rare
  enough that a surprise is rare, not frequent enough to chase upstream.
- **Write a local-time window with an EXPLICIT timezone, and verify the expression before installing.**
  `OnCalendar=Mon *-*-* 00:00:00 America/New_York` keeps a "Sunday night midnight" window at midnight
  across DST instead of sliding an hour; check it first with
  `systemd-analyze calendar "<expression>"` (it prints the normalized form and the next elapsed time in
  UTC, which is the fastest way to prove the window means what the owner asked for).
- **Prove the wiring, not just the schedule — ideally without performing the update.** Run the unit once by
  hand (that is what surfaces a standing refusal), or run the guard's read-only subcommand in the unit's own
  context and identity:
  `sudo systemd-run --uid=<service-user> --working-directory=<clone> --wait --collect <guard> preflight`.
  `systemd-analyze verify <unit>` checks syntax only — it will not tell you the guard refuses.
- **`hermes update --check` is the authority on "am I current", not a git commit count.** After a
  successful update the checkout can still sit N commits behind `origin/main`: those are unreleased
  commits beyond the STABLE channel the updater installs. A monitor that reports "N commits behind
  upstream main" therefore shows a red that no update will ever clear — the always-present warning that
  trains its reader to skip the channel. Compare against the updater's own verdict (or the stable tag),
  and if you must report the git distance, label it as unreleased.
- **Wiring that weekly run as a systemd unit — do NOT absorb exit 1.** `0` is success; `1` is a REFUSAL or
  a failed verify (and also any runtime error) and `2` is a usage error — every non-zero outcome is
  something a human must see. Leave `SuccessExitStatus` at its default (0 only) so a refusal surfaces as a
  RED unit that the host's agent layer escalates, instead of a `SuccessExitStatus=0 1` that makes a refused
  update read as a clean night. Use `Type=oneshot` with `WorkingDirectory` set to the built clone, and **no
  `Restart`** — a refusal will refuse again a minute later; next week is the retry.

## `fatal: remote error: upload-pack: not our ref <SHA>` — the STALE-REF failure mode

The guarded update fails at channel resolution with `Could not resolve the stable source channel:
git merge-base --is-ancestor <target> <our HEAD>` plus the git error above. It is **not a guard fault and not
a connectivity fault** — both were verified. Two conditions combine:

- **The install is a partial clone**: `remote.origin.promisor=true`,
  `remote.origin.partialclonefilter=blob:none`. A `blob:none` clone answers a *local* walk (like
  `merge-base`) by **fetching the missing objects on demand** — so a local operation becomes a network
  operation that can fail.
- **The local remote-tracking ref was STALE.** `origin/main` sat at an older SHA than live main, and a check
  that compares HEAD against the *local* ref therefore answers about a cached value, not about the remote.
  Stale ref + partial clone is the pairing that produces the refusal.

**Do not read the failure out of a NAME.** `git tag --contains HEAD` returning `abandoned-rc.*` tags is NOT
evidence that your commit was abandoned — those tags name abandoned *release candidates* of a release line.
The same HEAD measured `git merge-base --is-ancestor HEAD <live-main>` → **YES**: still an ancestor of live
main, and it always had been. The install was on the live line the whole time; the ref cache, not the commit,
was the problem. A tag or branch name is a claim about what upstream intended, never a measurement of
lineage.

**Diagnosis, in order — all read-only:**

```bash
cd <install-dir>
git fetch origin --tags --prune      # refresh the ref cache FIRST — it is a cache, not the remote
git rev-parse origin/main; git ls-remote origin refs/heads/main   # these two must now agree
git merge-base --is-ancestor HEAD origin/main; echo $?            # 0 = our HEAD is on the live line
git tag --contains HEAD              # context only — a name is not a verdict
git config --get remote.origin.partialclonefilter                 # blob:none explains the remote round-trip
```

**Fix (proven):** `git fetch origin --tags --prune`, then re-run the guarded update. After the refresh the
fetch of our own HEAD returned **exit 0** with HEAD unchanged, and the guarded update then succeeded — landing
on the very commit the channel resolver had been unable to reach, with `VERIFY: PASS`. Before the refresh the
same update refused; after it, the same update passed. That difference is the proof the fix is the ref
refresh, not a lucky retry.

**The transferable rule:** a remote-tracking ref is a **cache of remote state**, and a partial clone turns a
local walk into a remote fetch — so a stale cache converts a purely local operation into a failing network
one while `preflight`'s drift/venv gates read green. **A green preflight is not a green update**, and a stale
ref is **refreshed, never retried around and never re-cloned** — do not re-run the update hoping, and do not
conclude from a tag name that the commit was abandoned.

**The heal above is PROCEDURE, not code — the binary does not perform it.** No code path matches the
channel-resolution failure, so a guarded update that hits it stops with the updater's error and waits for a
human. Run the refresh by hand and re-run the guarded update; do not sit on a red unit expecting the binary
to repair itself, and never "fix" it by disabling the drift gate.

**Auditing whether a documented lesson is actually implemented: grep the SOURCE for the identifiers the code
would use, and read the function that would render the behaviour.** A miss is a question, not a finding —
searching for a display cap with `MAX_SHOWN` / `and N more` returned nothing while the cap was fully
implemented as `const SHOWN: usize = 25`, `take(SHOWN)` and a `suppressed` counter. Settle both directions
before reporting a gap, and when a lesson really is prose only, say which sentence is procedure and which is
code, so the next reader does not assume the binary does it. Measured consequence of getting this wrong: a
working feature reported as missing, one step away from rewriting it.

## What each verdict actually measures (v0.3.8)

Read the LABEL, not the exit code alone — each command names its reference in its
own output, and the three references are not interchangeable.

- **`diff`** → live surface vs the **approved baseline** (`golden.json`), falling back
  to `last-known/`. Prints `baseline: GOLDEN state`.
- **`verify`** → live surface vs the **same approved baseline**. It used to compare
  against `last-export.txt`, so an intentional `bless` made `diff` clean and `verify`
  FAIL on the same surface (G15). It now prints
  `no drift vs the approved baseline (golden.json)` and reports a difference from the
  pre-update export as a NOTE (`BLESSED changes, not update drift`), never as a FAIL.
  **If you ever see these two disagree again, that is the bug — not a judgement call.**
- **`export`** only advances `last-known` + the pointer; **`bless`** is the only thing
  that moves `golden.json`. After a deliberate config/model/key change: `export`, then
  `diff` (expect 0), then `bless` if the new surface is the approved one.

### The log-signature section is attribution, not a log dump

`verify` reports only signatures whose **newest line is newer than the snapshot**
(taken during the backup stage). Two consequences worth knowing before reading it:

- A rotated generation (`agent.log.2`) belongs to the `agent` stream, so rotation
  cannot rename a signature into looking new; and a signal whose lines all predate
  the snapshot is suppressed and **counted**, never silently dropped (G16).
- The window is `since the snapshot`, which on an old snapshot is wide: the section
  prints the snapshot's own timestamp and age for exactly that reason. Signals dated
  after the snapshot but hours after the update finished are post-update drift, not
  proof the update caused them.
- Display is capped at 25 signatures by count plus `... and N more`; the full set is
  the snapshot file. A 1,600-line report is a report nobody reads.

## Preflight: what a flag means

- `behind : 261 commits (detached; distance to update target <sha>)` — the updater
  leaves the install DETACHED at the tag it installs, so detached is the NORMAL
  post-update state. It never prints `n/a` any more (that read as "up to date" and
  hid a whole release, G17). `unknown (... target not in the local object store)`
  means run `git fetch --tags --prune` in the install directory and re-read.
- `[warn] .hermes-update-in-progress.lock present` — a WARNING. Hermes and the
  updater quarantine these themselves and the guarded update only warns; it does not
  block. Clear it with `quarantine --apply`. A zero-byte marker whose owner pid is
  unreadable is history.
- `[desktop app] RUNNING` — blocks the update phase unless `--close-desktop`.

## `self-check` exit 1 is usually a DATA finding, not a broken tool

`self-check` runs the fleet's context-health audit, so it exits 1 when any profile
has duplicate live rows, archive bloat ≥ 4x, or a session with a recorded
compression failure. Read the table before concluding the guard is unwell:

- `live/dup 1.00x, dupUIDs 0, bloat 5.0x` = historical archive waste (repeated
  compaction copied the same rows). The live transcript is clean; the DB is fat.
  `hermes sessions optimize` reclaims space without touching data; deduplicating the
  archived rows is a data mutation — back up first (`guard backup`) and get the
  owner's consent.
- `stuck N` = sessions carrying a `compression_failure_error` that was never cleared.
  A months-old session is history; it does not mean compression is broken now.
- The line `installed binary is at least as new as the repo build` compares against
  the repo's `target/`, so a repo whose code changed but was not rebuilt still reads
  green — build, then compare.

**Before repairing any of it, read `references/context-health-repair.md`** — which
column is a defect vs storage, the backup that over-1 GB DBs depend on, the trigger-safe
dedupe recipe, and why VACUUM needs the gateway stopped. A `bloat 5.0x` profile is not a
bad transcript: it is 4 dead archive copies per message.

## Contributing: `master` is protected

`git push origin master` is rejected (`GH006: Protected branch update failed`, 3
required status checks). Work goes through a branch + PR (`gh pr create`). A local
commit on `master` must be moved to a branch before it can be pushed
(`git checkout -b fix/<slug>` keeps it), and CI must pass on every platform before it
merges — a green local `cargo test` is not the gate.

- **`cargo fmt --check` is one of the gates. Run `cargo fmt` before pushing.** Formatting drift fails
  macOS, Linux and Windows in ~10 s, and the log reads like a compile failure — it is a diff. A green
  `cargo test` says nothing about it, so a local suite passing is not evidence the PR will.
- **Write the commit message and the PR body to SEPARATE files.** Feeding the same file to
  `git commit -F` and `gh pr create --body-file` makes the commit subject the body's first line
  (`## What this fixes`) — amend and force-push the branch before opening the PR, not after review.
- **A new classifier needs a test in BOTH directions.** A matcher shipped with only a positive case is
  the false-green this repo's audits keep finding: assert it fires on the real shapes AND does not fire
  on a neighbouring failure (a DNS error, a not-a-repo message, an unrelated refusal). A one-directional
  test passes whether or not the classifier is too broad.

### Landing a change and releasing it

`master` takes **merge commits, not squashes**: `gh pr merge <n> --merge --delete-branch`. Squash rewrites
the SHA a report was told about, and the history is a chain of "Merge pull request #N".

1. Branch off `master`, one concern per PR, with the `Cargo.toml` version and the CHANGELOG entry in the
   same PR. Wait on the PR's own run (`gh run watch <id> --exit-status`), not on a polling loop.
2. When one PR stacks on another, merge the base first: the stacked PR's diff then shrinks to its own
   commits instead of replaying the base in review.
3. Tag only from `master` after the merge: `git tag -a vX.Y.Z -m "..." && git push origin vX.Y.Z`. The
   release workflow's gate REFUSES a tag whose commit is not on `origin/master`.
4. The workflow builds three OS binaries into a **DRAFT** release. Publishing is a separate decision:
   `gh release edit vX.Y.Z --draft=false --latest`, then prove the asset downloads and its sha256 matches
   the digest from `gh release view vX.Y.Z --json assets`.
5. Seats install the artifact for their OS (Windows needs the `.exe` plus `WebView2Loader.dll`) or build
   from source, then run `self-check --repo <clone>`. That is what turns "which version is this seat
   running" into a reading instead of a memory.

Before promising a gate to another seat, know which platforms implement it **on the binary that seat is
running**. The mount proof and the volatile refusal were Linux-only up to 0.4.x; since 0.5.0 macOS reads
the `mount` table and Windows compares volume roots (see *Platform notes*). Ask the seat for
`hermes-update-guard --version` and `self-check --repo <clone>` before quoting a gate — a seat on an older
binary silently degrades to the permissive branch `is_mountpoint: true, fstype: unknown`.

## Decision rules

- **A pasted "final state: all green" report is a CLAIM, not a reading — re-run the
  read-only commands before continuing, and report the delta.** Resume work from a
  handover summary by measuring, not by trusting: `self-check`, `preflight`, `diff`,
  `verify`, `mcp`, `health` are all cheap and read-only. Measured on a summary whose
  every line was true when written: by the next session `self-check` had gone to exit
  1 (bloat + stuck sessions) and a fresh zero-byte update marker was sitting in the
  home. State moves between sessions; only a receipt dates it.
- **A scheduled job that failed BEFORE the update may just be old-gateway credential state — re-run it once before diagnosing.** The credential pool resolves provider tokens inside the running gateway, so a job can fail with `RuntimeError: HTTP 401: User not found.` for weeks and complete on the first try after the update's gateway restart. Measured: every weekly run for a month recorded `failed` while still writing its output file, then `completed` with `error: None` on the first re-run against the new pid. Re-run with `hermes --profile <p> cron run <job-id>` and read the receipts, not the console line: `cron/executions.db` (`status`, `error`), `cron/output/<job-id>/` for the artifact, and `usage_audit.jsonl` for tokens/model. Also read `delivery_outcome` — `completed` with `not_configured` means the run is real but reached nobody, which is a separate finding from a failure.
- **After ANY deliberate config, model or key change, re-run `export` before the next scheduled run.**
  The tripwire compares the LIVE surface against the pinned baseline, so a change you made on purpose
  reads as DRIFT and the guard refuses — correctly, but from the scheduler's side that refusal repeats on
  every run until someone re-pins. A scheduled weekly update can therefore go red every week and the host
  quietly stays on an old version. Finishing a provider/model/key change means: `export`, then confirm
  `diff --baseline last-known` exits 0. Run `preflight` and read the drift block — it names the exact keys
  that moved (`model.provider`, `auxiliary.*`, `moa`, …).
- `preflight` reports `dirty: N file(s)` → those files will be stashed by the
  updater. If a tracked core file is intentionally patched (for example
  `hermes_cli/models_catalog_static.py`), verify the stash list after the update
  and restore it if the update dropped it.
- `diff` exits 1 (drift found) → **do not update**; investigate `model.*`,
  `fallback_*`, `delegation`, and `display.language` first.
- `update` refuses to run while a venv holder or the Desktop app is running; pass
  `--close-desktop` only when the operator has already closed their session.
- Backup FAIL → never update from an unverified backup.

## Measured on the mcpvot seat, 2026-11-06 (v0.4.0, branch `feat/external-backup-target`)

Four gaps found by reading the source and dry-running it. Three shipped in **0.4.1** and were carried
forward through 0.4.2 and 0.4.3 on one branch; the fourth is the disk. Re-measure before trusting this:
`hermes-update-guard self-check --repo <clone>` reports the installed version and whether the binary is
behind the repo.

- ~~**The POSIX desktop probe cannot see the Desktop app on Linux.**~~ **FIXED 0.4.1.** `preflight.rs` asked for
  `Hermes.exe`; `env.rs:849` answers with `pgrep -f <image>`, and the Linux app's command line is
  `<checkout>/apps/desktop/release/linux-unpacked/Hermes --...`, which never contains `Hermes.exe`.
  Measured: `[desktop app] not running` printed while pid 12881 ran. So `--close-desktop` is a no-op
  here and the packaged rebuild overwrote the files of a running app. `env::desktop_app_images()` now
  names each platform's image, `combine_proc_states()` lets a positive match win while keeping `Unknown`
  fail-closed, and `close_desktop_app()` closes every match. Verified on the live seat: `[desktop app]
  RUNNING` where it used to print `not running`, so `--close-desktop` works on Linux now.
- ~~**A removable target that is NOT mounted can be accepted.**~~ **FIXED 0.4.1.** `prove_target_with` proves
  the *nearest existing ancestor*, not the target; `/run/media/<user>` exists as a tmpfs mount, so
  `facts_from_entries` (`env.rs:376`) answers "not on the root filesystem" and `is_mountpoint` is true.
  `UNSAFE_FSTYPES` (`env.rs:247`) lists vfat, exfat, iso9660 and has no tmpfs. Measured with the disk
  absent: `backup --dry-run --backup-root "/run/media/<user>/New Volume/..."` printed
  `device: run, fstype: tmpfs, mount: true, free 3.6 GB` and planned all three databases. The restore
  point would have been written to RAM and called fine. `VOLATILE_FSTYPES` (tmpfs, ramfs, devtmpfs) is
  now refused unconditionally, and `--allow-local-backup` does not waive it: that flag means the internal
  disk, never memory. Verified: the same unmounted path exits 1 naming `RAM-backed`, the control root
  still plans a complete backup, and the tests cover a fork-shaped target in both directions.
- **A present but dirty NTFS volume is still not a target.** `ntfs3(sda2): volume is dirty and "force"
  flag is not set` makes a read-write mount fail; udisks then mounts read-only. A disk listed in `lsblk`
  is not a usable backup root.
- ~~**`verify` attributes context health against a receipt of any age.**~~ **FIXED 0.4.1.** It used to read
  `RunState.health_before` with no run-id or age test, so a verify run 45h later compares against the
  previous run's baseline and prints `FAIL context health got WORSE during the update`. Measured that
  case: 402 dup uids in trilogy-brain, **0** same-session duplicates under the guard's own filter
  (`active = 1`, grouped by `message_uid, session_id`) - every one is a fork. `count_live_dup_uids()`
  now groups by `(message_uid, session_id)`, so a fork is not duplication and the same uid twice in one
  session still is (both directions tested); a baseline older than 6h prints `pre-update baseline is
  <n>h old` instead of being used for attribution. Verified: `verify` went from exit 1 to
  `VERIFY: PASS` exit 0 with `dup_uids=0` on all three profiles.

## Platform notes

- **The mount proof covers all three platforms since 0.5.0.** Linux uses `/proc/self/mountinfo`, macOS
  parses the `mount` table (an unreadable table fails closed), Windows compares volume roots so an
  absent drive letter is caught. Windows drive types feed the same lists: `cdrom` unsafe, `ramdisk`
  volatile, a mounted `removable` disk legal. A relative path has no volume identity and is refused.
  Nothing here replaces reading the proof line: `device`, `fstype` and `mount` are printed per target.

## Pitfalls

- **`venv state UNKNOWN (probe exit 0)` is `HERMES_HOME`, not a held venv.** The probe runs the
  checkout's `venv/bin/hermes update --list-venv-holders`. A reader that merges stderr into stdout sees
  the CLI's ~626-byte `[HERMES_HOME fallback]` warning instead of the payload when `HERMES_HOME` is unset
  on a multi-profile host, so the real `[]` no longer parses and the gate refuses a free venv. Measured:
  626 bytes of stderr unset, 0 bytes with `HERMES_HOME` set. **Fixed in 0.4.3**: the probe reads stdout
  only, passes `HERMES_HOME` to the child, and treats an empty stdout as free only on exit 0. On an older
  binary, export `HERMES_HOME` before the run.

- **A second updater on the same checkout is a fork in the truth, not a convenience.** Omarchy ships an
  agent update path: `omarchy-default-agent` answers `hermes` on an Omarchy seat, and its release notes
  say Hermes and OpenClaw agents are self-updating through it. Two updaters pointed at one checkout means
  the backup, the drift baseline and the verify receipt can describe a different run than the one that
  changed the code. Pick one owner per host — disable the desktop's agent-update action or route it at
  `hermes-update-guard update` — and say which, because "the agent updates itself" and "the guard updates
  the agent" cannot both be true on the same seat.
- **`hermes update --yes` is what keeps an unattended run from blocking**, and it is the flag the guard
  passes. Measured on v0.22.5 and v0.23.0: `--yes` accepts the config-migration and stash-restore
  prompts on both, so the guard needs no `--keep-stash` and no expect script. Before scheduling a run on a
  NEW version, read that version's `hermes update --help` — a prompt `--yes` does not cover hangs the run
  until the timeout, and the release notes will not name it as a breaking change.
- **Updating with the desktop app open** → the update refuses; close the app (or
  pass `--close-desktop`) and retry.
- **Ignoring a `diff` drift** → you lose the guarantee that the update keeps your
  model/provider config; resolve drift first.
- **Trusting a partial backup** → the `backup` gate already refuses incomplete
  backups; do not override it.
- **Stale lock after a previous failed update** → `preflight` surfaces it; clear
  it before retrying. The marker is safe to clear once it is proven stale — zero bytes, an mtime well
  older than the last run, and no updater process alive (check with `pgrep -x <binary>`, never `pgrep -f`
  on a string your own command line contains). Leftover locks the guard does NOT flag — `hermes-update.lock`,
  `hermes-pack-tidy.lock` inside `.git` — are the tool's own bookkeeping: prove git still works
  (`git status`) and leave them alone rather than tidying another tool's state.
- **"Could not resolve the stable source channel" is NOT a connectivity failure — separate network from
  refs state before touching anything.** The updater resolves the channel by asking git to compare the
  target against your HEAD, so the error text names git while the cause can be either end. Measure both, in
  this order: (1) network — `curl -sI https://github.com` for a status and a time, and `git ls-remote origin`,
  which must list refs; (2) object presence — `git cat-file -t <HEAD>` for the local object, then the raw
  command the updater ran (`git merge-base --is-ancestor <target> <HEAD>`). A **`fatal: remote error:
  upload-pack: not our ref <HEAD>`** means the remote does not recognise your installed commit at all, which
  is a refs/clone-state problem that no retry, and no amount of connectivity, will clear — and the install
  is typically a **partial clone** (`git remote -v` shows `[blob:none]`), which is why its object store and
  the remote's refs can disagree about the same install. Report which of the two it is before proposing a
  remedy; a refresh of refs (`git fetch origin --tags --prune`) is the first thing to try, and **it is now
  proven**: the same update that refused went through after the refresh, landing on the commit the resolver
  could not reach (`VERIFY: PASS`). Record a repair as the working method once a run proves it — this one has.
- **A FAILED update still advances the drift baseline.** The pre-update `export` step runs before the update
  and re-pins `last-known`, so after a refusal the original backup directory is the restore point — the
  guard says so in its failure output — while the baseline now describes the pre-update live surface. Read
  the printed backup path, and do not treat an advanced baseline as evidence that an update happened.
- **A MISSING baseline is an absent reading, not a clean pass.** `diff --baseline golden` on a host with no
  `golden.json` printed `no provider-surface drift detected` and exited 0 in 0.7.0 and earlier. Measured
  against a temp home with no baseline: false PASS. The absent-reading law applies to the guard itself, not
  only to the surfaces it watches. **Fixed in 0.7.2**: `diff` refuses a baseline path that does not exist and
  names it; `baseline_surface` errors instead of returning `None`.
- **A live `config.yaml` that fails to parse is not "a profile with no drift".** The compare walk called
  `extract_surface` and skipped the profile on `Err`, so a corrupt config produced no row and a clean exit.
  **Fixed in 0.7.2**: the walk prints `live surface UNREADABLE` and counts it as a failed reading. The
  UNREADABLE counter now matches both baseline and live lines, so the row reaches the failure branch.
- **An equal context-health reading is not a "PRE-EXISTING condition".** `verify` printed the warning
  whenever the before/after summaries were equal, including on a fleet with zero duplicates, zero bloat and
  zero stuck sessions. A warning on every clean run is wallpaper. **Fixed in 0.7.2**: the warning fires only
  when the equal reading is actually a problem; a healthy unchanged run prints the `ok` line.
- **`--home` must reach every baseline resolution.** `resolve_baseline` called `env::hermes_home()` instead
  of the resolved home, so `--home /tmp/x diff --baseline golden` read the REAL `~/.hermes` baseline while
  reporting on the temp home's profiles. **Fixed in 0.7.2**: the resolver takes the resolved home.
- **A Windows path in a Linux hint is a false lead.** The self-check "stale clones" hint printed
  `home\\tools`; on this seat the path is `home/tools`. Fixed in 0.7.2.

## When the host also has an agent-aware desktop (Omarchy)

Measured on this seat 2026-11-07. A desktop that integrates Hermes is a second way to update it.

- **Omarchy runs no updater of its own.** Migration `1790017600.sh` retired the mise-built wrapper with
  the note "Hermes now updates itself", and `omarchy-default-agent` is `hermes`. No user timer, cron
  entry or script here invokes `hermes update`. The second path is Hermes' passive checker plus the
  Desktop's Update button.
- **Turn the passive prompt off where the guard owns updates.** `hermes config set updates.check false`
  in the root home and every profile: the one-click unguarded path is not offered, and
  `hermes update --check` still answers (measured, same session).
- **Keep a backup on the hand-run path.** `updates.pre_update_backup: quick` in every home: one seat
  here had `false`.
- A packaged `hermes-desktop` beside the checkout is a separate problem: hide the stale launcher, never
  let the packaged app's update path own the verified checkout.
- On a guarded seat `hermes update` runs only through `hermes-update-guard update`; the button, the
  typed command and any vendor nudge are one unguarded path under three names.

## Manual restore (emergency)

```bash
rsync -a "$HOME/.hermes/backups/pre-update-<stamp>/<profile>/state.db" \
      "<HERMES_HOME>/<profile>/state.db"
```

The same applies to `.env`, `auth.json`, and `config.yaml`. Restore only after
stopping the gateway (`hermes gateway stop`) and closing Hermes.

## Verification: a test must not depend on the filesystem it runs on

- **A test must not change verdict with the filesystem it happens to run on.** `/tmp` on this host is
  tmpfs, and the volatile refusal correctly rejects a tmpfs target, so a fixture that built a bulk root
  under `TMPDIR` with `--allow-local-backup` passed on CI (disk temp) and failed here. Since 0.4.3 that
  test asserts the RAM-backed refusal on a volatile target and the retention behaviour otherwise, so the
  suite gives the same verdict both ways. Keep that shape when adding tests: assert the refusal where the
  refusal is correct instead of skipping, and prove the suite under a disk temp
  (`TMPDIR=~/.cache/tmp cargo test`) as well as under the default.

## Verification

- `hermes-update-guard verify` exits `0` and reports DB integrity `ok` with no drift
  versus the **approved baseline** (it prints which baseline it used), and names the
  snapshot age for the log-signature section.
- Since 0.7.0 it READS the restore point instead of trusting its manifest: each snapshot the manifest
  lists is opened read-only and integrity-checked (message count printed, or `unknown`), and when
  secrets were included `.env` and `auth.json` must exist and be non-empty per profile. A manifest
  saying COMPLETE over a truncated snapshot or a missing secret fails the verdict.
- Since 0.6.0 it also prints the `.hermes-update*` markers left in the home (split into
  held by a LIVE run and left behind by a dead one, with `quarantine --apply` as the way
  out) and the gateway's served/parked profile map. Neither is a failure. Measured on this
  seat: two markers survived the v0.23.0 run and were only found by hand, and a PASS with a
  parked profile is still a fleet with a member down. Read those lines before quoting a PASS.
- Exit codes: `0` ok · `1` drift found, backup incomplete, verify failure, or any runtime error ·
  `2` CLI usage error (bad flags/arguments) · **`3` data finding** (0.3.9+: `self-check`,
  `context-health`, `mcp`). Before 0.3.9 a data finding and a tool failure were both `1`, so a
  scheduled run could not tell them apart.

## Audit 2026-10-26 — findings, fixed and shipped in 0.3.9 (PR #7, merged `ad30450`)

**0.3.9 is merged and installed.** CI green on macOS, Ubuntu and Windows (fmt + tests + release
builds). Verified on this machine: `self-check` exits **3** (data finding), `preflight` exits 0.

Full write-up: `docs/HERMES-UPDATE-GUARD-AUDIT-2026-10-26.md` in the CRM repo. 64/64 tests pass after the fix; these were the gaps
the 0.3.8 tests did not cover. Test the list against a newer build before believing it.

- ~~**A failed section renders as an empty one.**~~ **FIXED 0.3.9** - now `[FAIL] ... could not run` and it affects the verdict. `verify.rs:331`, `main.rs:221`, `context_health.rs:320`,
  `preflight.rs:287` all discard a section's Result. If log-signature or MCP reporting errors, the header
  prints and nothing follows — which reads as "found nothing". Treat a MISSING section as a failure, not a
  clean result.
- ~~**The channel-resolution failure is unhandled.**~~ **FIXED 0.3.9** - refresh refs once, retry once, refuse if the refresh fails. Before 0.3.9 no code path matched `Could not resolve the stable
  source channel` / `upload-pack: not our ref`, and the refresh-and-retry repair was manual only.
  Do not mistake `prefetch_tags` for a mitigation: it refreshes tags **for the version label only** (Hermes'
  own tag refresh has a 30 s budget that timed out on this machine), it is fired in the background, not
  waited on, and its failure is deliberately non-blocking.
- ~~**Exit 1 means both "found something" and "the tool broke".**~~ **FIXED 0.3.9** - data findings exit 3. Before 0.3.9 a healthy tool exited 1 for three stale compression-stuck
  sessions in *other* profiles, and a scheduled unit could not tell that from a crash. Read the table, not the code, before escalating.
- ~~**Seven `repo.to_str().unwrap()`**~~ **FIXED 0.3.9** - lossy conversion, no panic. (`backup.rs:166,173,180,189`; `preflight.rs:69,75,87,115`) panic on a
  non-UTF-8 repo path mid-update instead of refusing with a message.

- **A session FORK is reported as duplicated rows (M5; fixed in 0.4.1, still true of older binaries).** `dup_uids` grouped by
  `message_uid` alone, so the same message living once in each of two forked sessions counts as a
  duplicate: measured **627** on ofcrm with **zero** duplicate rows in any profile. `verify` therefore
  goes red after every update for normal history. Treat a `dup_uids` FAIL as a claim to MEASURE before
  believing it, and never delete sessions to clear it. Replicate the guard's OWN predicate first —
  `context_health.rs` counts `WHERE active = 1` grouped by `message_uid` — then group the same rows by
  `(message_uid, session_id)`: zero same-session rows means forks only and the FAIL is the artifact.
  Dropping the `active = 1` filter counts compacted and archived copies and INVENTS same-session
  duplicates that do not exist (measured: 116 unfiltered vs 0 with the guard's predicate, same DB).
- **A `verify` run long after an update reports usage, not the update.** Its context-health section
  compares against the `health_before` block stored in that run's receipt, so forks created hours or
  days later print `context health got WORSE during the update`. Read the receipt id and timestamp in
  the same output before escalating: a fresh run re-measures "before" and the same surface passes.

Verified healthy, so do not re-flag: backup `pragma integrity_check` + message counts (`backup.rs:251,260`),
secret masking on export (`drift.rs:32-80`), desktop probe failing closed on Unknown (`preflight.rs:227-234`),
argv-only command construction, and the venv-holder gate.
