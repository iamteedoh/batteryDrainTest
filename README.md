# batteryDrainTest

[![CI](https://github.com/iamteedoh/batteryDrainTest/actions/workflows/ci.yml/badge.svg)](https://github.com/iamteedoh/batteryDrainTest/actions/workflows/ci.yml)
![License](https://img.shields.io/badge/license-GPL--3.0-blue)
[![GitHub Sponsors](https://img.shields.io/badge/GitHub%20Sponsors-%E2%9D%A4-ea4aaa?logo=githubsponsors)](https://github.com/sponsors/iamteedoh)
[![Patreon](https://img.shields.io/badge/Patreon-support-f96854?logo=patreon)](https://patreon.com/iamteedoh)
[![Buy Me a Coffee](https://img.shields.io/badge/Buy%20Me%20a%20Coffee-support-ffdd00?logo=buymeacoffee&logoColor=black)](https://buymeacoffee.com/iamteedoh)

A terminal-based battery drain testing and monitoring tool built in Rust. It
intentionally drains a laptop battery by pinning every CPU core while showing
a live TUI of battery percentage, drain rate, CPU/memory usage, and
temperatures — and logs everything to CSV for later playback.

The application lives in the [`battery-drainer/`](battery-drainer/) crate. See
[battery-drainer/README.md](battery-drainer/README.md) for full documentation:
features, architecture, UI layout, controls, and the CSV log format.

<p align="center">
  <img src="docs/hero.png" alt="Battery Drainer live dashboard: a BATTERY DRAINER banner above real-time battery, CPU, and memory gauges and a time-series chart" width="100%">
</p>

## Quick start

```bash
git clone https://github.com/iamteedoh/batteryDrainTest.git
cd batteryDrainTest/battery-drainer

# Drain mode (unplug the charger first)
cargo run --release

# Plot mode: replay a recorded log
cargo run --release -- --plot drain_log_YYYYMMDD_HHMMSS.csv
```

Press `q` to quit either mode.

## Safety

Drain mode maximizes CPU usage and heat generation on purpose. Thermal
protection relies on your OS and hardware limits — monitor temperatures, and
avoid extended runs, which can accelerate battery wear.

## License

This project is licensed under the [GNU General Public License v3](LICENSE).

## Contributing

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md) for local
setup, the validation suite, and the pull request process.

## Security

To report a vulnerability privately, follow [SECURITY.md](SECURITY.md). Do not
open public issues for security problems.
