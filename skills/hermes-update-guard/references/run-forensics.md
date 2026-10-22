# Reading a guarded run backwards

How to turn one finished run into numbers, and which numbers are already known.

## Measure before optimising

`verify` carries an opt-in section timer:

```sh
HUG_TIMING=1 hermes-update-guard verify
```

It prints `[timing] <section>: N.Ns` per section. Use it before claiming any
speedup; two separate estimates in this repo were wrong by 4-6x.

## Measured on run `20261021-201542` (Windows, 6 profiles, 4.7 GB of state.db)

Whole run **32m32s**: preflight 36s, export 0.2s, backup 2m31s,
**update 25m21s (78%)**, verify 3m41s.

Inside `hermes update` (the guard cannot shorten these): git fetch/stash/pull
~5m, node deps ~4m, web UI + desktop build ~14m, gateway restart ~1m18s.

`verify` breakdown (198s, after the 0.3.7 parallel change):

| section | time |
|---|---|
| head+version | 11.9s |
| **databases** | **148.6s** |
| patch+drift | 0.5s |
| context health | 24.7s |
| gateway+receipt | 5.3s |
| log signals | 5.5s |

## Database integrity is I/O-bound, and that is the floor

Per-DB `PRAGMA integrity_check`, read-only, measured on this fleet:

| profile | size | integrity_check | quick_check |
|---|---|---|---|
| vot | 2.13 GB | 150.1s | 107.6s |
| ofcrm | 640 MB | 32.5s | - |
| default | 425 MB | 23.7s | - |

- **Parallelising saved ~10%** (148.6s vs ~164s serial) with 8 cores: six readers
  contend for one disk. It is still worth keeping (free), but do not expect more.
- **`quick_check` saves only 28%** (150s -> 108s on vot) while skipping index
  verification. Not a good trade for a fail-closed gate: the guard exists to
  prove integrity, not to feel fast. Do not swap it in silently; if a `--fast`
  mode is ever added, it must be explicit and documented as weaker.
- The log scan is NOT the cost (442 files / 212 MB -> 5.5s). An earlier guess that
  logs dominated was wrong; the timer settled it.

## The gateway churn on Windows is usually self-inflicted

`hermes update` warns `Windows gateway restart could not be verified ... may have
been killed by a parent Job Object (#48820)`. On this host the ledger showed the
successor process `--replace`d the one the guard had just observed alive, and the
real cause was timing: the updater judged liveness after ~2.5s against a ~78s
boot. Two consequences:

1. the guard now requires the same pid alive on two probes 8s apart, and waits
   up to 120s for a restart;
2. never restart the gateway in parallel with a guard run - you will kill the one
   the updater just spawned and blame the wrong component.

## Stale version label after an update

`hermes update` refreshes tags on a 30s budget. When that times out it writes a
label from stale tags (`0.22.2+3030.g4d04303` while the tree was the v0.22.4
commit) and says the label may be stale. The install is fine; `verify` compares
`code_sha`, which is the fact that matters. The guard now pre-fetches tags during
the backup so the label lands correctly. If you meet it on an older build:
`git -C <home>/hermes-agent fetch --tags`, then correct `baseVersion`,
`distance`, `tag` and `displayVersion` in `install-stamp.json` (back it up first -
it is a generated file, and the next update rewrites it).
