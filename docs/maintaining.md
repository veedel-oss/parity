# Maintaining Parity

Toolchain, builds, releases and the settings on GitHub, for the maintainer. How Parity works is in [architecture.md](architecture.md).

## Toolchain

`rust-toolchain.toml` pins one Rust version, with rustfmt, clippy and the musl target, for every build: CI, the e2e tests, releases and local development. It is the only supported Rust version. It changes only with a commit, and Dependabot proposes updates after a 7-day wait. So the release binary is built with the toolchain the e2e tests ran with.

## Static release build

The release binary is built for `x86_64-unknown-linux-musl` and is fully static. No C code is compiled: `rust-lld` from the pinned toolchain links the binary against the musl of Rust's target, not the system's linker (`.cargo/config.toml`).

| Flag | Why |
| --- | --- |
| `-C target-feature=+crt-static` | static even once Rust makes dynamic linking the default for musl ([rust-lang/compiler-team#422](https://github.com/rust-lang/compiler-team/issues/422)) |
| `-C link-self-contained=yes` | the musl and start files of the pinned toolchain, never a musl installed on the build machine |

A `RUSTFLAGS` from the environment replaces these flags without an error. CI therefore denies warnings with `CARGO_BUILD_WARNINGS=deny` (Cargo's `build.warnings`), not with `RUSTFLAGS=-D warnings`.

Everything built for the build machine itself is linked with its `cc`: development builds and tests, and the build scripts and proc macros of the dependencies. So building needs a C compiler.

CI builds the binary and checks that it is static and smaller than 5 MB.

## Releases

Pushing a signed tag runs `.github/workflows/release.yml`:

```bash
git tag -s v<version> -m "Release v<version>"
```

Tag only a commit on `main` on which `ci.yml` and `e2e.yml` passed: the release workflow does not run the tests. Until the release, the section of the version in `CHANGELOG.md` is headed `## <version> (unreleased)`, without a date. The tagged commit drops the marker, otherwise the build job stops.

| Job | What it does | Access |
| --- | --- | --- |
| build | builds the binary, checks that it is static, that `parity --version` matches the tag and the changelog heading, and builds the Debian package | read only, no OIDC token, no stored credentials, as it runs the build scripts of the dependencies |
| release | after the maintainer's approval in the environment `release`: attests the build provenance of the binary and the package, and creates a draft release | write, but runs no code of the project |

The draft release holds:

| File | Content |
| --- | --- |
| `parity-<version>-x86_64-unknown-linux-musl` | the static binary |
| `parity_<version>_amd64.deb` | the Debian package |
| `SHA256SUMS` | the checksums of both |
| release notes | the section of `CHANGELOG.md` for the version |

The maintainer publishes the draft. The provenance can be checked with `gh attestation verify <file> --repo veedel-oss/parity`.

## Debian package

`packaging/deb/build.sh BINARY OUTDIR` packages a static binary as it is, with `dpkg-deb`. The release builds the package from the binary it built and checked, and checks that the binary in the package is byte-identical to it, so the package holds the tested bytes. The script works the same locally; `make build-deb` runs it in a container from `packaging/deb/Dockerfile`, for hosts without dpkg-deb. dpkg-deb comes with dpkg on every Debian and Ubuntu system, so the release installs no packaging tool.

| The package | Why |
| --- | --- |
| installs `/usr/bin/parity`, `changelog.gz`, `copyright` and an `md5sums` file for `dpkg --verify`, nothing else | a package must not install into `/usr/local`; `/usr/bin` passes the holder's ownership checks and is on the SSH user's `PATH` and in sudo's `secure_path`, so `--parity-path` keeps its default |
| has no completions and no man pages | `parity completion` prints the completions, `--help` is the reference |
| has no maintainer scripts, conffiles or services | installing configures nothing, removing leaves nothing behind |
| has no Depends or Recommends | the binary is static; the workstation needs an ssh client, a target an SSH server and sudo or su, which one package for both roles cannot express |
| has the version the binary reports, without a Debian revision | releases are immutable |
| is compressed with xz | dpkg before 1.21.18 (Debian 10 and 11) cannot unpack zstd |
| has `SOURCE_DATE_EPOCH` as every file time, by default the time of the commit | the same commit gives the same package |

## License notices

`packaging/deb/copyright`, in Debian's machine-readable format, names everything linked into the static binary with its copyright holders and license: Parity (AGPL-3.0-or-later with the full text, which `/usr/share/common-licenses` does not have), every crate from crates.io, the Rust standard library, LLVM's libunwind and musl. CI checks that it lists exactly the crates of `cargo tree -p parity --target x86_64-unknown-linux-musl -e normal,no-proc-macro`.

## Settings on GitHub

The maintainer sets these on GitHub, not in the repository, so that only the maintainer can publish a release:

- Organisation: the maintainer is the only owner, the base permission gives members no write access, and two-factor authentication is required.
- Repository: nobody else has write, maintain or admin access. Releases are immutable.
- Rulesets: only organisation owners can push `v*` tags. The CI jobs are required status checks, so renaming a job needs a change of the ruleset.
- Actions: only the actions the workflows use are allowed, those of GitHub, `astral-sh/ruff-action` and `hadolint/hadolint-action`, each pinned to a full-length commit SHA. The default `GITHUB_TOKEN` is read-only and cannot create or approve pull requests, and workflows from fork pull requests of external contributors need approval. Only the release job has write access.
- Security: Dependabot alerts and security updates, Secret Protection (secret scanning and push protection) and private vulnerability reporting are enabled.
