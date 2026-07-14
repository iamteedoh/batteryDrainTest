# Contributing to batteryDrainTest

Thanks for helping improve batteryDrainTest. This guide covers local setup,
validation, and the pull request process.

## Ways to contribute

- **Report a bug** using the repository's bug report form.
- **Request a feature** using the feature request form.
- **Send a pull request** after opening an issue for non-trivial changes.
- **Report a vulnerability privately** by following [SECURITY.md](SECURITY.md).

## Prerequisites

- Rust toolchain (stable) installed via [rustup](https://rustup.rs/)
- gitleaks 8.30.1 or newer
- A laptop with a battery, for exercising drain mode manually (plot mode works
  on any machine with a recorded CSV log)

## Set up from a clean clone

```bash
git clone https://github.com/iamteedoh/batteryDrainTest.git
cd batteryDrainTest/battery-drainer
cargo build
```

The crate lives in the `battery-drainer/` subdirectory, so run all `cargo`
commands from there. Never commit `drain_log_*.csv` output, `.env` files, or
anything containing environment-specific values.

## Run the validation suite

Run the same checks that protect `main`. From `battery-drainer/`:

```bash
cargo build --verbose
cargo test
```

From the repository root:

```bash
gitleaks git . --config .gitleaks.toml --redact --no-banner
```

When changing runtime behavior, exercise the affected mode locally: run drain
mode on battery power for a few minutes, or replay a recorded log with
`cargo run -- --plot <file>`. Remember that drain mode intentionally pins
every CPU core — keep an eye on temperatures while testing.

## Project layout

- `battery-drainer/Cargo.toml` — crate manifest
- `battery-drainer/src/main.rs` — the entire application: CLI parsing, drain
  mode (CPU load, live TUI, CSV logging) and plot mode (CSV playback)
- `battery-drainer/README.md` — detailed usage, architecture, and CSV format
- `.github/workflows/` — source validation and source-only release automation

## Pull request process

1. Create a branch from `main`.
2. Make the smallest complete change and update documentation.
3. Run the full validation suite above.
4. Use a [Conventional Commit](https://www.conventionalcommits.org/) PR title:
   `feat:`, `fix:`, `docs:`, `refactor:`, `ci:`, `test:`, or `chore:`.
5. Complete the pull request template and link the related public issue.
6. Wait for all required checks to pass, then squash-merge.

The PR title becomes the squash commit subject and drives release-please:
`fix:` creates a patch release, `feat:` creates a minor release, and a `!` or
`BREAKING CHANGE:` footer creates a breaking release.

## License

By contributing, you agree that your contributions are licensed under the
project's [GNU General Public License v3](LICENSE).
