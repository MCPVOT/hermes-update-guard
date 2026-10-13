# Security Policy

## Reporting a vulnerability

Please **do not open a public issue** for security problems. Report privately
to the maintainer listed on the repo owner's profile, or via GitHub's
private vulnerability reporting (Security → "Report a vulnerability") once the
repo is published.

## What this tool touches

- Reads (never writes) each Hermes profile's `config.yaml` to extract a
  provider *surface*; API keys are masked (truncated) **before** being written
  to `backups/provider-surface/`.
- Copies `.env` / `auth.json` byte-for-byte into `backups/pre-update-*/` — the
  same behavior as the legacy PowerShell wrapper. These backups live inside
  `HERMES_HOME/backups/`, so they inherit the same access controls as the
  original secrets.
- Invokes `hermes update --yes`, `hermes update --check`, and `git` — it never
  reimplements update logic and never touches live state except through those
  official binaries.

## Guarantees

- No secrets are ever printed to stdout/stderr or written to logs.
- No live config or `.env` is modified by any subcommand.
- The tool never phones home; it has no network calls of its own.

## Supported

Mainline release binaries built by the release workflow (Windows/Linux/macOS).