# Security Policy

## Reporting a vulnerability

**Do not report security vulnerabilities through public GitHub issues.**

Use GitHub's private vulnerability reporting instead:

1. Open the repository's **Security** tab.
2. Select **Report a vulnerability**.
3. Provide the details requested below.

If private reporting is unavailable, contact the maintainer through the
[iamteedoh GitHub profile](https://github.com/iamteedoh).

## What to include

- A description of the issue and its potential impact
- Reproduction steps or a minimal proof of concept
- The affected release, commit, platform, and component
- A suggested remediation, if known

Never include live tokens, passwords, SSH keys, private hostnames, or
unredacted logs in a report.

## Security-sensitive areas

batteryDrainTest is a local terminal tool that intentionally stresses the CPU,
so the most sensitive surfaces are:

- Parsing of CSV log files supplied via `--plot`, which may come from
  untrusted sources
- CSV log files written to the current working directory in drain mode
- The intentional CPU-load and heat generation behavior — thermal protection
  relies entirely on OS and hardware limits
- Battery, temperature, and system metrics read through the `battery` and
  `sysinfo` crates
- The crates.io dependency tree and CI/release automation

## Supported versions

Security fixes land on `main` and ship in the next tagged source release. Test
against the latest release or `main` before reporting an issue.
