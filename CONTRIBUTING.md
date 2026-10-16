# Contributing to Desk Bridge

Thanks for helping! Desk Bridge is a Windows-first Rust + Tauri project. This page is the short
version; [docs/development.md](docs/development.md) has the details.

## Ground rules

* Be kind: this project follows the [Code of Conduct](CODE_OF_CONDUCT.md).
* Discuss large changes in an issue first. [`docs/PLAN.md`](docs/PLAN.md) is the source of truth;
  a change of direction needs an ADR ([`docs/adr/`](docs/adr/README.md)).
* Security problems: do **not** open a public issue, see [SECURITY.md](SECURITY.md).
* By contributing you agree that your work is licensed under **MIT OR Apache-2.0**, like the
  project (no CLA).

## Setup

Rust stable (edition 2024), Node 24, MSVC build tools and CMake (for libopus). See
[docs/development.md](docs/development.md), including the cmake `PATH` note.

## Workflow

1. Fork, create a branch (`feature/short-name` or `fix/short-name`).
2. Make the change with tests. Anything platform-specific goes behind a trait in
   `crates/platform` with a mock; no Windows types outside `crates/platform-windows`.
3. Run the same checks as CI before pushing:

   ```powershell
   cargo fmt --all
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   cargo deny check
   cd app; npm run build
   ```

4. Update `CHANGELOG.md` (section *Unreleased*) and docs if behaviour changes.
5. Open a pull request using the template. Small, focused PRs are reviewed faster.

## Style

* Code, comments and commit messages in English; UI strings German (English translation later).
* `rustfmt` and `clippy -D warnings` are enforced. Libraries: `thiserror`; binaries: `anyhow`.
  No `unwrap()` on I/O or network results.
* Hot paths (input hooks, audio callbacks, video pipeline) must not block, allocate unboundedly or
  take locks that other threads hold for long. Low-level hook callbacks only enqueue.
* Commit messages: imperative subject line (<= 72 chars), body explains *why*.
* Protocol changes follow the versioning rules in [docs/protocol.md](docs/protocol.md): additive
  only, never reuse a protobuf tag, add a compatibility test.
* New dependencies must be shippable inside the installer and pass `cargo deny` (no GPL in the
  core). Anything that cannot be bundled or installed silently needs an ADR (plan 12.1).

## Testing hardware-dependent changes

CI cannot test mouse/keyboard injection, audio devices, drivers or GPU encoding. Mention in the PR
which items of [docs/testing.md](docs/testing.md) you ran manually, on which Windows version and
hardware.

## Reporting bugs

Use the bug report template and attach the log files from
`%LOCALAPPDATA%\softwelop\DeskBridge\logs\` (they contain no keys; check them for personal data
such as host names before posting).
