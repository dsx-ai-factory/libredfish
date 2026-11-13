# Contributing to libredfish

Use [GitHub issues](https://github.com/dsx-ai-factory/libredfish/issues) for usage
questions, bug reports, and proposed changes. Search existing issues and pull
requests first. For a substantial API or vendor behavior change, describe the
problem and proposed approach before you implement it.

Report security vulnerabilities privately through [SECURITY.md](SECURITY.md).
Do not include credentials, private keys, internal addresses, or sensitive BMC
inventory in public reports, logs, or fixtures.

## Development setup

1. Install Git and Rust through rustup. The repository pins Rust **1.90.0** in
   [rust-toolchain.toml](rust-toolchain.toml).
2. Install Python 3 with working `venv` and `pip` support for the integration
   tests. On Debian or Ubuntu, the relevant packages are `python3-full` and
   `python3-pip`.
3. Clone your fork, create a branch, and install the check tools:

   ```sh
   git clone https://github.com/<your-account>/libredfish.git
   cd libredfish
   git checkout -b <change-name>
   rustup component add clippy rustfmt
   ```

Cargo fetches Rust dependencies when you build. The integration test harness
creates a Python virtual environment in the system temporary directory,
installs [tests/requirements.txt](tests/requirements.txt), and starts local
HTTPS mock servers. Allow access to the Python package index and ensure ports
8732 through 8745 are available. Tests do not need a live BMC.

## Build and test

Run the checks used by [CI](.github/workflows/ci.yml) from the repository root:

```sh
cargo build
cargo fmt --check
cargo clippy --locked --all-targets --all-features --workspace -- -D warnings
cargo test
```

`cargo build` creates the local `Cargo.lock` used by the Clippy command; this
repository does not track that file. To check the usage example explicitly:

```sh
cargo check --example service_root
```

Use `cargo test --lib` for a focused unit test pass while you develop.
Before submitting a pull request, run the full checks above and report checks
that you could not run. The integration suite includes an explicitly ignored
Vera Rubin test; an ignored test is not evidence of hardware validation.

## Make a focused change

- Add a regression test for a behavior fix. Start with a failing test that
  shows the problem; do not weaken an existing test to make the change pass.
- Keep common operations in the standard implementation and add vendor
  differences in the corresponding module. Explain required OEM behavior and
  the BMC model and firmware used to verify it.
- Prefer sanitized fixtures from the affected platform when a response shape
  changes. The mock server checks paths for write requests, but it does not
  validate their bodies or prove that hardware applies the requested change.
- Update the README or API comments when public behavior changes.

## Submit a pull request

Sign off each commit with `git commit -s` to certify the
[Developer Certificate of Origin](https://developercertificate.org/), as checked
by the repository's [DCO configuration](.github/dco.yml).

Open a pull request against `main`. State the problem, the resulting behavior,
and test commands with their results. Include relevant hardware validation and
any limitations. Keep unrelated changes separate and remove sensitive data
before you submit.

The [Code of Conduct proposal](CODE_OF_CONDUCT.md) is pending owner confirmation
of its terms, reporting channel, and enforcement responsibilities before merge
and adoption.
