<!-- Thanks for contributing. See CONTRIBUTING.md; for larger changes, please open an issue first. -->

## What and why

<!-- What does this change, and why? Link the issue, for example "Closes #123". -->

## How it was tested

<!-- The commands you ran and the systems you tested on. If you could not run the e2e tests, say so. -->

## Checklist

- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass
- [ ] e2e tests run locally (`tests/e2e/run.sh <os>`), or the reason why not is given above
- [ ] README, help texts and `docs/architecture.md` describe the change, where needed
- [ ] A user-visible change comes with one line for `CHANGELOG.md`
- [ ] No new dependency in `parity-collect`, or the reason is given above
- [ ] Collect mode stays read-only and only collects raw data
- [ ] No company-specific or personal data: no real host names, addresses, user names or configuration
