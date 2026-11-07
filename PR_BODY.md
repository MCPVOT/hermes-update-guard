## What this fixes

Three leftovers from the **v0.22.7 -> v0.23.0 guarded update (2026-11-06)** that the log printed in plain sight
and the code could not act on.

### 1. The gateway line could misreport a live gateway as down

`verify` read `gateway_state.json`, saw `state=stopped`, and printed:

> `! gateway is recorded as state=stopped (pid=38876); nothing is serving - not a failure if you stopped it deliberately`

**pid 38876 was alive and serving the whole time.** The state file was stale, and the reassurance it produced
tells an operator their gateway is down when it is not. A state file is a CLAIM; the pid is the measurement.
The non-running branch now probes the pid through a pure predicate, `state_file_is_stale(state, pid, alive)`,
and says the file is stale when the process lives.

### 2. `a partial restore?` was a question the tool could answer itself

> `! 1 stash entry also present (a partial restore?)`

The stash list already names every entry, and the operator had to go dig to find out which patch was where
(it was the NVIDIA catalog hunk, present BOTH in the working tree and in `stash@{0}` from an earlier update).
The warning now prints each stash entry and the exact `git stash show -p stash@{N}` to inspect it.

### 3. `.hermes-update-paused-gateways.lock` was invisible

`preflight` and the guarded `update` each scanned a **fixed pair** of marker names
(`.hermes-update-in-progress`, `.hermes-update-in-progress.lock`). The update left
`.hermes-update-paused-gateways.lock` behind, so the only file actually present was never reported — and it
was still there the next morning. Both paths now use one `update_markers(home)` prefix scan, which is what
`quarantine` already did for exactly this reason.

## Evidence

Live, on this machine after the update:

```
[locks / interrupted markers]
  [warn] .hermes-update-in-progress.lock present ...
  [warn] .hermes-update-paused-gateways.lock present ...      <- new: was invisible before

  !    1 stash entry also present (a partial restore?) - READ these before answering:
       stash@{0}: On (no branch): hermes-update-autostash-20261104-022557     <- new: named
       ... inspect it with `git stash show -p stash@{0}` and drop it only after confirming the tree has the change
```

## Tests (both directions, per this repo's rule)

- `finds_the_leftover_and_ignores_neighbours` — positive: the `paused-gateways` marker IS found; negative: `.env` is not, and exactly 2 markers are reported.
- `stale_file_is_caught_when_the_pid_lives` — positive: `stopped` + live pid fires.
- `it_does_not_fire_when_the_gateway_really_is_down` — negative: dead pid, `pid=0`, and the already-handled `running` branch do not fire.

`cargo test`: **67 passed, 0 failed** (was 65). `cargo fmt --check` clean.

## Not in this PR

The `dup_uids=627` context-health FAIL (session forks counted as duplicate rows) is a separate, documented
false positive; it is unchanged here.
