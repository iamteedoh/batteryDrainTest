# Battery Drainer

A terminal-based battery drain testing and monitoring tool built in Rust. This application intentionally drains your laptop battery by maximizing CPU usage while providing real-time visualization of battery status, system metrics, and drain rate analytics.

## Features

- **Big, Beautiful UI**: A block-font splash title with a description, and a
  rounded, color-coded live dashboard header
- **Interactive & Non-Interactive**: A full-screen TUI by default, or a
  `--headless` mode that streams plain-text status to stdout (ideal for SSH/CI)
- **CPU Load Generation**: Spawns CPU-intensive threads on all available cores
- **Pause/Resume the Load**: Toggle the CPU burn on the fly without leaving the
  dashboard (`space` / `p`)
- **Monitor-Only Mode**: `--no-load` watches natural battery drain without
  adding any CPU load
- **Auto-Stop & Summary**: `--duration <MINUTES>` ends the run automatically and
  prints a session summary (drained %, average rate, peak temps)
- **Configurable Threads & Log Path**: `--threads <N>` and `--output <FILE>`
- **Real-time Monitoring**: Battery percentage, drain rate, CPU/memory usage, temperatures
- **Live Visualization**: TUI-based charts and gauges using Ratatui
- **Data Logging**: Automatic CSV logging for later analysis
- **Playback Mode**: Review historical data with VCR-like controls
- **Drain Rate Calculation**: Rolling 30-second window for accurate measurements
- **Time Estimation**: Estimates remaining battery life based on current drain rate

## How It Works

### CPU Load Generation

The application creates maximum CPU load by spawning one thread per CPU core, each running an infinite loop of floating-point calculations:

```rust
for _ in 0..num_cpus::get() {
    thread::spawn(move || {
        let mut x = 0.0f64;
        loop {
            x = (x + 1.0).sqrt();
        }
    });
}
```

This method:
- Utilizes 100% of all CPU cores
- Generates significant heat
- Drains the battery at maximum rate
- Allows measuring worst-case battery life

### Monitoring Stack

| Component | Source | Refresh Rate |
|-----------|--------|--------------|
| Battery % | `battery` crate | 1 second |
| Drain Rate | Calculated (30s window) | 1 second |
| CPU Usage | `sysinfo` crate | 1 second |
| Memory Usage | `sysinfo` crate | 1 second |
| CPU Temperature | `sysinfo` Components | 1 second |
| Battery Temperature | `battery` crate | 1 second |

## Architecture

```mermaid
flowchart TB
    subgraph Main["Main Entry Point"]
        A[Parse CLI Args] --> B{Mode?}
    end

    subgraph HeadlessMode["Headless Mode (--headless)"]
        B -->|Headless| HA[Print Banner] --> HB[Spawn CPU Threads]
        HB --> HC[Tick Loop: log + stream status]
        HC --> HD[Print Session Summary]
    end

    subgraph DrainMode["Drain Mode"]
        B -->|Interactive| SP[Show Splash Title] --> C[Spawn CPU Threads]
        C --> D[Initialize App State]
        D --> E[Setup Terminal TUI]
        E --> F[Event Loop]

        subgraph EventLoop["Event Loop (1s tick)"]
            F --> G[Refresh System Info]
            G --> H[Read Battery Status]
            H --> I[Calculate Drain Rate]
            I --> J[Log to CSV]
            J --> K[Render UI]
            K --> L{Key Press?}
            L -->|q| M[Cleanup & Exit]
            L -->|Other/None| F
        end
    end

    subgraph PlotMode["Plot Mode (--plot)"]
        B -->|Plot| N[Load CSV File]
        N --> O[Parse Log Entries]
        O --> P[Initialize Playback State]
        P --> Q[Setup Terminal TUI]
        Q --> R[Playback Loop]

        subgraph PlaybackLoop["Playback Loop"]
            R --> S[Render Chart]
            S --> T{Key Press?}
            T -->|Space| U[Toggle Play/Pause]
            T -->|v| V[Toggle View Mode]
            T -->|+/-| W[Adjust Speed]
            T -->|Arrows| X[Skip Forward/Back]
            T -->|q| Y[Cleanup & Exit]
            T -->|None| Z[Advance Playback]
            U --> R
            V --> R
            W --> R
            X --> R
            Z --> R
        end
    end

    subgraph DataFlow["Data Flow"]
        direction LR
        AA[Battery Manager] --> BB[LogEntry]
        CC[System Info] --> BB
        DD[Components] --> BB
        BB --> EE[CSV Writer]
        BB --> FF[UI Renderer]
    end
```

## Dependencies

```toml
[dependencies]
ratatui = { version = "0.26.0", features = ["all-widgets"] }
crossterm = "0.27.0"
battery = "0.7"
num_cpus = "1.16.0"
clap = { version = "4.5.4", features = ["derive"] }
sysinfo = "0.30.12"
chrono = "0.4.38"
csv = "1.3.0"
serde = { version = "1.0", features = ["derive"] }
```

## Installation

### Prerequisites

- Rust toolchain (1.70+)
- A laptop with a battery

### Build from Source

```bash
# Clone the repository
git clone https://github.com/iamteedoh/batteryDrainTest.git
cd batteryDrainTest/battery-drainer

# Build release version
cargo build --release

# The binary will be at ./target/release/battery-drainer
```

## Usage

### Command-Line Options

| Option | Description |
|--------|-------------|
| `-p, --plot <FILE>` | Replay a previously recorded CSV log instead of draining |
| `-H, --headless` | Run without the TUI; stream status to stdout (SSH/CI friendly) |
| `-d, --duration <MINUTES>` | Stop automatically after N minutes (`0` = run until you quit) |
| `-t, --threads <N>` | Number of CPU load threads (default: one per logical core) |
| `--no-load` | Monitor the battery only — spawn no CPU load threads |
| `-o, --output <FILE>` | Log file path (default: `drain_log_<timestamp>.csv`) |
| `-h, --help` | Print help |
| `-V, --version` | Print version |

### Drain Mode (Default)

Run the application to start draining the battery and monitoring:

```bash
cargo run --release
```

Or run the compiled binary:

```bash
./target/release/battery-drainer
```

On launch you'll see a big block-font **BATTERY DRAINER** splash with a short
description; press any key (or wait a moment) to drop into the live dashboard.

Useful variations:

```bash
# Stop automatically after 15 minutes and print a session summary
./target/release/battery-drainer --duration 15

# Only monitor natural drain — do not add CPU load
./target/release/battery-drainer --no-load

# Use 4 load threads and a custom log path
./target/release/battery-drainer --threads 4 --output my_run.csv
```

#### Drain Mode UI Layout

```
┌─────────────────────────────────────────────────────────────────┐
│ Time: 14:32:45  |  CPU Temp: 72.5°C  |  Battery Temp: 38.2°C    │
│ Drain Rate: 1.25%/min  |  Avg: 1.18%/min  |  Est. Remaining: 1h │
├─────────────────────────────────────────────────────────────────┤
│ Battery [████████████████████████░░░░░░░░░░░░░░░░░░] 62.5%      │
├─────────────────────────────────────────────────────────────────┤
│ CPU [████████████████████████████████████████] 98.2%            │
│ Memory [██████████████░░░░░░░░░░░░░░░░░░░░░░░░] 42.1%           │
├─────────────────────────────────────────────────────────────────┤
│                     Real-time Analysis                           │
│ 100%│ ─────────────────────────────────────────                 │
│     │      ╲                                                     │
│  50%│       ╲____  Battery %                                    │
│     │            ╲___                                           │
│   0%└──────────────────────────────────────────                 │
│     0s          30s          60s         90s                    │
├─────────────────────────────────────────────────────────────────┤
│ B% | DR | CPU | MEM || Logging to: drain_log_20260114.csv       │
└─────────────────────────────────────────────────────────────────┘
```

#### Drain Mode Controls

| Key | Action |
|-----|--------|
| `space` / `p` | Pause / resume the CPU load (dashboard keeps running) |
| `q` | Quit and save log |

### Headless (Non-Interactive) Mode

For servers, SSH sessions, or CI where a TUI isn't practical, run with
`--headless`. It prints the title banner, streams a compact status line every
10 seconds, and prints a session summary when it stops:

```bash
# Drive the battery down for 10 minutes with no TUI
./target/release/battery-drainer --headless --duration 10
```

```
[0h 00m 10s]  Battery  82.0%  Drain  0.00%/min (avg  0.00)  CPU  99%  Mem  41%  CPU  72°C  Batt 38°C
...
==================== Session Summary ====================
Duration:        0h 10m 00s (600 samples)
Battery:         85.0% -> 71.0%  (drained 14.0%)
Avg drain rate:  1.40 %/min
Est. full drain: 0h 50m 42s
Peak CPU usage:  99%
Peak CPU temp:   88°C
Peak batt temp:  39°C
=========================================================
```

Without `--duration`, headless mode runs until you press `Ctrl-C` (the CSV is
flushed every second, so no data is lost). Provide `--duration` for a clean,
automatic stop with a printed summary.

### Plot Mode (Playback)

Review previously recorded data:

```bash
cargo run --release -- --plot drain_log_20260114_143256.csv
```

#### Plot Mode Controls

| Key | Action |
|-----|--------|
| `v` | Toggle Static View / Playback Mode |
| `Space` | Play / Pause |
| `+` or `=` | Speed up (2x, 4x, 8x... up to 64x) |
| `-` | Slow down (0.5x, 0.25x) |
| `←` | Rewind 10 samples |
| `→` | Fast-forward 10 samples |
| `Home` | Jump to start |
| `End` | Jump to end |
| `r` | Reset speed to 1x |
| `q` | Quit |

#### View Modes

**Static View**: Shows all recorded data at once - useful for seeing the complete picture.

**Playback Mode**: Replays the recording as if it were happening in real-time. The chart grows progressively and current values are displayed.

## CSV Output Format

The application logs data to a CSV file with the following columns:

| Column | Type | Description |
|--------|------|-------------|
| `timestamp` | f64 | Seconds elapsed since start |
| `percentage` | f32 | Battery percentage (0-100) |
| `drain_rate` | f32 | Drain rate in %/minute |
| `cpu_usage` | f32 | CPU usage percentage |
| `memory_usage` | f32 | Memory usage percentage |
| `cpu_temp` | f32 | CPU temperature in Celsius |
| `battery_temp` | f32 | Battery temperature in Celsius |
| `clock_time` | String | Wall clock time (HH:MM:SS) |

### Sample CSV Output

```csv
timestamp,percentage,drain_rate,cpu_usage,memory_usage,cpu_temp,battery_temp,clock_time
0.0,85.5,0.0,2.3,41.2,45.0,32.0,14:32:45
1.0,85.5,0.0,98.5,41.3,52.0,32.5,14:32:46
2.0,85.5,0.0,99.1,41.3,58.0,33.0,14:32:47
...
35.0,85.0,0.86,98.7,41.5,72.0,38.0,14:33:20
```

## Code Structure

```
battery-drainer/
├── Cargo.toml          # Project manifest
├── Cargo.lock          # Dependency lock file (committed for reproducible builds)
├── README.md           # This file
└── src/
    └── main.rs         # Single-file application
```

### Key Data Structures

```rust
/// A single record for logging
struct LogEntry {
    timestamp: f64,
    percentage: f32,
    drain_rate: f32,
    cpu_usage: f32,
    memory_usage: f32,
    cpu_temp: f32,
    battery_temp: f32,
    clock_time: String,
}

/// Application state for drain mode
struct App {
    start_time: Instant,
    battery_manager: battery::Manager,
    log_writer: csv::Writer<File>,
    data: Vec<LogEntry>,
    log_filename: String,
    system: System,
    components: Components,
}

/// Playback state for plot mode
struct PlaybackState {
    position: usize,
    playing: bool,
    speed: f64,
    last_tick: Instant,
    static_view: bool,
}
```

## Drain Rate Calculation

The drain rate uses a **30-second rolling window** for stability:

```rust
// Find a sample from ~30 seconds ago
let target_time = elapsed_seconds - 30.0;
let reference_entry = self.data.iter()
    .rev()
    .find(|e| e.timestamp <= target_time)
    .unwrap_or(&self.data[0]);

// Calculate rate based on the window
let time_diff_secs = elapsed_seconds - reference_entry.timestamp;
let percent_diff = reference_entry.percentage - percentage;
let drain_rate = (percent_diff / time_diff_secs * 60.0) as f32; // %/min
```

This approach provides stable readings because:
- Battery percentage typically only updates every 30-60 seconds at the OS level
- Comparing shorter intervals would show 0% most of the time
- The rolling window ensures continuous, meaningful measurements

## Tips for Accurate Testing

1. **Unplug the charger** before starting
2. **Close other applications** to isolate the drain test
3. **Disable power saving features** for maximum drain
4. **Run for at least 10-15 minutes** to get meaningful data
5. **Note the starting battery level** for calculating total capacity

## Safety Considerations

- This application intentionally maximizes CPU usage and heat generation
- Monitor temperatures - most CPUs will throttle above 90-100°C
- The application does not implement thermal protection beyond OS/hardware limits
- Running for extended periods may reduce battery lifespan over time

## License

This project is licensed under the [GNU General Public License v3](../LICENSE).

## Contributing

Contributions are welcome! See [CONTRIBUTING.md](../CONTRIBUTING.md) for local
setup, the validation suite, and the pull request process. To report a
security issue privately, follow [SECURITY.md](../SECURITY.md).
