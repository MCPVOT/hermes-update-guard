# Security Policy

## Reporting a vulnerability

Please **do not open a public issue** for security problems. Instead:

1. Use GitHub's private vulnerability reporting — **Security → Report a
   vulnerability** on this repository, or
2. Contact the maintainer listed on the
   [repo owner's profile](https://github.com/MCPVOT).

We aim to acknowledge reports within a few days and will credit reporters unless
they ask otherwise.

## Supported versions

| Version | Supported |
|---|---|
| 0.1.x (latest release) | yes |
| older pre-release tags | no |

Only mainline release binaries built by the release workflow
(Windows / Linux / macOS) are supported.

## What this tool touches

- **Reads** (never writes) each Hermes profile's `config.yaml` to extract a
  provider *surface*; API keys are masked (truncated) **before** being written to
  `backups/provider-surface/`.
- **Copies** `.env` / `auth.json` byte-for-byte into `backups/pre-update-*/`.
  These backups live inside `HERMES_HOME/backups/`, so they inherit the same
  access controls as the original secrets.
- **Invokes** `hermes update --yes`, `hermes update --check`, and `git`. It never
  reimplements update logic and never touches live state except through those
  official binaries.

## Guarantees

- No secrets are ever printed to stdout/stderr or written to logs.
- No live config or `.env` is modified by any subcommand.
- The tool never phones home; it has no network calls of its own.

## Scope

This policy covers the `hermes-update-guard` binary and the code in this
repository. It does not cover Hermes Agent itself — report those issues to the
[Hermes Agent project](https://github.com/NousResearch/hermes-agent).
