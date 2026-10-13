# Contributing

Thanks for your interest in `hermes-update-guard`. This is a small, focused
tool: a guardian wrapper around `hermes update`. Contributions that keep it
small, safe, and cross-platform are very welcome.

## Ground rules

- **Never commit secrets.** No `.env`, `auth.json`, tokens, or keys — in code,
  tests, fixtures, or docs.
- **One concern per pull request.** Keep diffs reviewable.
- **Don't break the gates.** `cargo fmt --check`, `cargo clippy --all-targets --
  -D warnings`, and `cargo test` must all pass.

## Development setup

```bash
git clone git@github.com:MCPVOT/hermes-update-guard.git
cd hermes-update-guard

cargo fmt --check                            # format gate
cargo clippy --all-targets -- -D warnings    # lint gate
cargo test                                   # unit tests
cargo build --release                        # static-CRT standalone exe on Windows
```

The pinned toolchain is `stable` (see `rust-toolchain.toml`).

## Branch naming

```
fix/description        # bug fixes
feat/description       # new features
docs/description       # documentation
test/description       # tests
refactor/description   # code restructuring
```

## Commit messages

Follow the Hermes/Conventional-Commit style:

```
type: concise subject line

Optional body explaining the "why".
```

Types: `fix:`, `feat:`, `refactor:`, `docs:`, `test:`, `chore:`.

## Pull request process

1. Branch from `master`.
2. Make your change; add or update tests where behaviour changes.
3. Run the three gates locally (see above).
4. Open the PR with a short description: **what** changed and **why**.
5. CI runs fmt/clippy/test plus a release build across Windows, Linux, and
   macOS.

## Releases (maintainers)

1. Bump `version` in `Cargo.toml`, add a `CHANGELOG.md` entry, commit.
2. Tag and push: `git tag -a vX.Y.Z -m "hermes-update-guard vX.Y.Z" && git push origin vX.Y.Z`.
3. The Release workflow builds the three OS binaries and leaves a **draft**
   GitHub Release. Review the artifacts, then publish it from the UI.

## License

By contributing, you agree that your contributions are licensed under the MIT
License (see [LICENSE](LICENSE)).
