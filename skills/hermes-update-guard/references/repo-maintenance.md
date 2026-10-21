# Maintaining and releasing the guard (Rust repo)

How to change the tool itself, as opposed to running it. Load this when the task is
"fix / improve / release hermes-update-guard", not "run an update".

## Where things live

| What | Where |
|---|---|
| Repo | `MCPVOT/hermes-update-guard` (public, branch `master`); paths below are relative to your local checkout |
| Installed binary (Windows) | `%LOCALAPPDATA%\hermes\bin\hermes-update-guard.exe` |
| Installed binary (droplet) | `~/.local/bin/hermes-update-guard` on `hoodbot@100.111.194.19` |
| Skill mirrors | `profiles/mcpvot/skills/devops/hermes-update-guard/` locally, plus `hoodbot` and `sentinel` on the droplet |

The droplet has **no Rust toolchain** — it can only run a binary downloaded from a
published release, never a local build. This is why a release that is left as a draft
makes the droplet unusable.

After editing the repo's `skills/hermes-update-guard/SKILL.md`, copy it to every
mirror (`scp` to the droplet, `cp` to the profile path) or the installed skill keeps
describing commands the installed binary already has.

## Gates: run all three before any push

CI runs exactly these on ubuntu, macos and windows:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The release workflow additionally runs `cargo build --release`.

A successful `cargo build` proves nothing about the other two. Run the gate set
verbatim before committing; `fmt --check` in particular fails on whole-file
formatting that a local `cargo fmt` run never applied.

## Cross-platform clippy trap

A helper reached only from Windows code paths is `dead_code` on Linux and macOS, and
`-D warnings` promotes that to a hard error: the local Windows build is green while
`check (ubuntu-latest)` and `check (macos-latest)` fail. Keep the helper compiled on
every platform — its unit tests are pure and run anywhere — and annotate it:

```rust
#[cfg_attr(not(windows), allow(dead_code))]
```

Do not delete the function and do not gate it with `#[cfg(windows)]`: that removes
its tests from the platforms where they are cheapest to run. The local target cannot
observe this lint, so **the CI matrix is the only oracle** — never report a
cross-platform fix as verified before the Linux and macOS jobs pass.

## An optional GUI stays out of the CLI's build

A desktop window for the guard (Tauri/WebView) belongs in its own crate that is
**outside the Cargo workspace and outside `[package]`**:

```toml
# root Cargo.toml
[package]
exclude = ["dashboard"]
```

A WebView dependency drags platform system libraries — `webkit2gtk` on Linux — into
every build that includes it, and the `check` job runs on ubuntu, macos and windows.
A GUI crate in the same workspace turns a green CLI into a red matrix that has
nothing to do with the CLI. Build it on demand, ship it separately, and keep
`cargo build` at the root meaning "the CLI, nothing else".

Two rules that follow: emit the UI as static files so the window needs no Node
toolchain, and keep the crate strictly read-only against the guard's own output
(`backups/guard-run.json` + the log). A viewer that can write is a viewer that can
strand a run — the same contract the `watch` TUI follows.

## Branch protection: master takes PRs only

`master` requires the three `check` jobs, so a direct `git push origin master` is
rejected with "required status checks are expected". The loop:

```bash
git checkout -b fix/<topic>
git push -u origin fix/<topic>
gh pr create --base master --head fix/<topic> --title ... --body ...
gh run watch <run-id> --exit-status       # wait for all three OS jobs
gh pr merge <n> --merge --delete-branch
```

Use `--merge`, not `--squash`, when the SHA reported to the user must stay the commit
they were told about; squash rewrites it. `gh run watch` on the PR's run is the wait
— polling `gh pr checks` in a loop just burns turns.

## Release flow

```bash
git tag -a vX.Y.Z -m "vX.Y.Z - <headline>"
git push origin vX.Y.Z          # Release workflow builds 3 binaries into a DRAFT
gh release edit vX.Y.Z --draft=false --latest
```

Publishing is a separate decision from tagging: the workflow always leaves a draft,
and a draft's asset URLs are `untagged-...` and not what anyone should be given.

Prove the artifact is public before saying the release exists — download it:

```bash
curl -sL -o /tmp/asset -w "%{http_code} %{size_download}\n" \
  https://github.com/MCPVOT/hermes-update-guard/releases/download/vX.Y.Z/<asset>
sha256sum /tmp/asset                     # compare with the asset digest
gh release view vX.Y.Z --json assets     # digest lives here
```

On Windows, pass native `C:/...` paths to `curl` and `sha256sum`: a bare `/tmp`
resolves differently for MSYS tools than for native ones, and the file lands
somewhere the next command cannot see.

## A version bump touches four files

`Cargo.toml` (`version`), `CHANGELOG.md` (new section **and** the compare links at
the bottom), `README.md` (`VER=` in the install snippet), and
`skills/hermes-update-guard/SKILL.md` (frontmatter `version`).

`edition = "2021"` is the Rust language edition, not the release version — leave it
alone when bumping.

When inserting a CHANGELOG section, read the heading immediately below the insertion
point first; a careless insert leaves two `## [x.y.z]` headings for the same version.

## Keep `PROFILES` operator-agnostic

`env::PROFILES` is a last-resort fallback and must stay `["default"]`. The repo is
public and must not carry one operator's profile names; the real fleet is enumerated
from disk by `env::profiles`. Any check that needs "which profiles should exist"
must derive it from data the guard already wrote — for example the newest
`pre-update-*` backup — never from a hardcoded list, which is wrong on every other
host (the droplet's fleet is entirely different).

## When a check flags an operator decision, record the decision

A new anomaly check earns its keep by firing. It stops being useful the moment it
fires on something the operator already decided — the report stays amber forever and
people learn to skip it, which is worse than not having the check at all.

Do not weaken the check, and do not put a name allowlist inside it. Add a RECORDED
decision beside it, the same shape as `bless`: `retire <profile>` writes
`backups/profile-retired.json` with a date and the last backup that saw the profile,
`--undo` removes it, and the check consults that record.

Two properties make the record a decision rather than a mute button:

- **It refuses to apply to something still live.** `retire` bails when the profile
exists on disk, so it can never hide a real one. Hiding a live profile from the fleet
checks is the exact failure the check exists to catch, so the escape hatch must not be
able to become the bug.
- **It is dated and reversible.** The absence becomes an auditable decision instead of
an unexplained gap, and `--undo` restores the flag immediately if the profile comes
back (e.g. after a restore from backup).

The general rule: state that represents a JUDGEMENT — this profile is gone on purpose,
this surface is approved — moves only through an explicit operator command that records
when and why. Never as a side effect of a probe, and never by loosening the check that
surfaces it.

## Reviewing another agent's landing

Run the gates, then run the new commands against the live home. Reading the diff is
not enough: a subcommand that compiles and passes unit tests can still move a live
update's lock or silently exclude a profile. Two checks have already caught real
defects this way — treat `fmt --check` and a manual run of every new subcommand as
mandatory review steps, not optional polish.

### Start with `git status`, not with the diff

An agent session can end mid-implementation and leave its work **uncommitted**. A
dirty tree means an unfinished hand-off, not a delivered feature: build it, gate it,
and finish the wiring before auditing anything, or every finding is against code
that was never meant to be final.

The session transcript records INTENT; the repository records OUTCOME. When they
disagree the repository wins. If the operator hands you a session export, mine it for
its claims ("this catches the OAuth cascade"), then verify each claim against the
repo and the live install — that is where you find the half-built module the last
agent never got to test.

### A new flag that changes no output is a defect

Wiring is the part that silently goes missing. A flag can be declared in the CLI,
parsed into the enum, and never passed to the function that uses it, so `--flag`
exits 0 and prints exactly what it printed before. For every new flag: grep its call
site, then run it and diff the output against a run without it. Check ordering too —
a command that calls `process::exit` before printing the extra report the flag asked
for produces the same nothing.

### Checks that lie (the classes worth grepping a diff for)

Every one of these produced a confident wrong answer rather than a crash:

- **A check gated on the wrong field.** Judging an OAuth token for every server
  flags `auth: none` / stdio servers, which have no token file by design. Gate the
  verdict on the same field that makes the condition meaningful.
- **A lookup that reads one store.** Tokens live under `profiles/<p>/mcp-tokens`
  before the root store; a root-only read reports a real, present credential as
  "missing". Resolve most-specific-first, then fall back.
- **A line-scan over structured data.** Scanning a JSON file line by line finds
  nothing in a single-line or reformatted file and defaults to `ok` — the fail-open
  direction. Parse it, and treat a parse failure as unknown, never as valid.
- **Existence used as a quality signal.** A lock file existing does not make it
  stale; that is an age test. Same shape as pid-checking `gateway_state.json`
  instead of trusting the word "running".
- **A char-boundary panic in Rust.** `s[a..b]` on a `String` built from chars, and
  `String::truncate(n)`, both panic when the offset is not a char boundary, so any
  log line containing non-ASCII text takes the tool down. Slice and truncate through
  `chars()`.
- **A pattern that only matches one format.** Requiring `" ERROR "` with spaces on
  both sides misses `ERROR:`, `[ERROR]` and `level=ERROR`. Match on tokens.
- **A fixed-index parse of a field whose layout you assumed.** A timestamp parser
  demanded `:` at index 10, where a real line carries a space or `T`, so it never
  matched: every record reported an empty `last_seen` and nothing errored. Parse by
  shape, and assert the parser against one literal line copied from the live log
  before trusting anything derived from it.
