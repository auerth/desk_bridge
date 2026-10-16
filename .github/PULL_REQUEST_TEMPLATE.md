## What and why

<!-- Short description. Link the issue / plan section / ADR. -->

## Type of change

- [ ] Bug fix
- [ ] Feature / milestone work (M_)
- [ ] Refactoring / docs / CI
- [ ] Protocol change (additive only? compatibility test added?)

## Checklist

- [ ] `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` pass
- [ ] `cargo deny check` passes (new dependencies are shippable in the installer, no GPL in the core)
- [ ] `cd app && npm run build` passes (if the UI is touched)
- [ ] Platform-specific code is behind a `crates/platform` trait with a mock
- [ ] Tests added or updated; hot paths do not block
- [ ] `CHANGELOG.md` (Unreleased) and docs updated
- [ ] ADR added if this deviates from `docs/PLAN.md`

## Manual testing

<!-- Which items of docs/testing.md did you run, on which hardware / Windows version?
"Not applicable" is fine for pure refactorings. -->

## Screenshots / logs

<!-- Optional -->
