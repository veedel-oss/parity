# Contributing to Parity

Issues and pull requests are welcome. For anything larger than a small fix, please open an issue first, so we can agree on the approach before you spend time on it. This matters most for changes to collect mode, the protocol, or the command line.

Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).

## Reporting bugs

Please include:

- the version (`parity --version`; the CLI and the binary on the targets must be the same version)
- the distribution and kernel of the workstation and of the targets
- the command you ran, its output and its exit code (0 identical, 1 differences found, 2 error)
- how privileges were escalated (`--become sudo`, `su` or `none`)

Reports can contain host names, paths and file contents. Remove anything you would not want to publish before you post it.

## Reporting security issues

Do not open a public issue for a security problem. The [security policy](SECURITY.md) says how to report it privately, and what counts as a vulnerability.

## Before you start

Read [docs/architecture.md](docs/architecture.md). It explains how the CLI and collect mode work together and lists the decisions made so far, with their reasons. [AGENTS.md](AGENTS.md) has the same rules in short form.

Some principles are fixed, and changes that break them will not be merged:

- Parity never changes the systems it inspects. Collect mode runs as root, is read-only, writes no files, leaves access times unchanged and exits when the session ends.
- Collect mode only collects raw data and sends it back. Parsing, comparison and reports belong in the CLI (`parity-analyze`), which runs without privileges.
- All communication runs over the stdin and stdout of the SSH session, using the system's `ssh`. No port forwarding, no embedded SSH library.
- Passwords for sudo or su never pass through Parity. They are typed on the user's terminal.

Open an issue first for:

- a new dependency of `parity-collect`. Everything there runs as root on the target, so every dependency needs a reason.
- a change to the protocol (`parity-proto`)
- a new, renamed or removed command or option. The names are fixed since the first release, and renaming one breaks the scripts that use it.
- support for a further distribution's package database. Collect mode sends the raw database files and the CLI parses them, as for dpkg today.

Additions to the built-in list of sensitive paths (`crates/parity-collect/src/sensitive.rs`) and to the default excludes (`crates/parity-analyze/src/lib.rs`) are welcome as pull requests. Explain why the path holds secrets or always differs between machines.

## Development setup

You need Linux, [rustup](https://rustup.rs) and a C compiler such as `gcc`. Rust uses it as the linker for everything built for your machine itself: `cargo build`, `cargo test`, and the build scripts and proc macros of the dependencies. Only the musl release binary is linked without it. `rust-toolchain.toml` pins the Rust version that every build uses, in CI, for releases and locally. rustup installs it together with rustfmt, clippy and the musl target the first time you run `cargo` in the repository, or ahead of time with:

```bash
rustup toolchain install
```

The code must also build with Rust 1.85, the minimum supported version (`rust-version` in `Cargo.toml`). CI checks that with `cargo +1.85 build --workspace --all-targets --locked`.

The checks CI runs on every pull request:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --release --locked --target x86_64-unknown-linux-musl
```

## Tests

- Unit tests are pure. They get what they need (paths, UIDs, metadata, file system types) passed in or mocked, and never depend on the system that runs them.
- Integration tests in `crates/*/tests` only touch fixtures they create themselves.
- Whatever only a real system can show (kernel behaviour, access times, `/proc`, real ssh, sudo and su) belongs in the e2e tests in `tests/e2e`.

The e2e tests boot two virtual machines from the latest Debian cloud image and test against them over real ssh:

```bash
tests/e2e/run.sh debian-12     # or debian-10, debian-11, debian-13
```

They need docker with access to `/dev/kvm`, plus `ssh`, `ssh-keygen`, `curl` and `python3`. The images are downloaded once and cached in `~/.cache/parity-vm`. CI runs them for all four releases on every pull request. If you cannot run them locally, say so in the pull request.

Shell scripts must pass `shellcheck` without findings. Disable a check only where the code is intended, on that line and with the reason, for example `# shellcheck disable=SC2016 # expands on the VM`.

## Debian package

The release builds the Debian package with [cargo-deb](https://github.com/kornelski/cargo-deb) (`.github/workflows/release.yml`), from the binary that CI and the e2e tests test; nothing else builds it. Its configuration is `[package.metadata.deb]` in `crates/parity/Cargo.toml`. To build it locally as the release does, install the cargo-deb version that `packaging/deb/tools/Cargo.toml` pins (Dependabot keeps it up to date there), then run `cargo xtask deb`:

```bash
cargo install cargo-deb --version =<version> --locked
cargo xtask deb                 # or: cargo xtask deb OUTPUT
```

It builds the static binary, generates the man pages and shell completions into `packaging/deb/generated`, runs cargo-deb on that binary and prints the path of the package, by default in `target/debian/`. A plain `cargo deb` fails, because it does not generate the man pages and completions, and it would package a dynamically linked binary for the build machine. `crates/xtask` is the tool for the release, never shipped; `cargo xtask` is an alias for `cargo run --locked -p xtask --` (`.cargo/config.toml`). `packaging/deb/copyright` names the copyright holders and licenses of everything linked into the binary. CI checks that it lists exactly the crates linked into it, so a pull request that adds or removes a dependency of the binary updates the file too.

## Code guidelines

- Collect mode must keep working on old kernels, down to Linux 3.2. Use newer system calls only with a fallback. The system calls it uses are listed in `docs/architecture.md`.
- Collect mode does no name lookups. It sends UIDs and GIDs as numbers, and the CLI maps them to names.
- Never implement hashing, cryptography, random number generation or other security-relevant algorithms yourself. Use well-known, maintained crates (SHA-256 from `sha2`, random tokens from `rand`).
- Write the code yourself for this repository. Do not copy code from other projects.
- No company-specific data anywhere: no real host names, IP addresses, user names or configuration, not in code, tests, examples or commit messages. Use names like `host-a` or `vm1`, and the documentation addresses `192.0.2.0/24` and `example.com`.
- Keep the documentation in step with the code: the README, the help texts of the CLI and `docs/architecture.md`. Describe user-visible changes in one line for `CHANGELOG.md`.

## Pull requests

- One topic per pull request. Small pull requests are reviewed faster.
- Branch from `main` and keep the history linear: rebase onto `main` instead of merging `main` into your branch.
- Commit messages have a short subject in the imperative ("Match excludes as patterns") and a body that explains what changed and why, wrapped at 72 characters.
- Describe how you tested the change, especially on real systems.
- All checks must pass: formatting, clippy, the tests, the build with Rust 1.85, the static builds and the e2e tests.

## License

Parity is licensed under the [GNU Affero General Public License v3.0 or later](LICENSE). By contributing, you agree that your contribution is licensed under the same terms.
