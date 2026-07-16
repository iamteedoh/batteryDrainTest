// SPDX-License-Identifier: GPL-3.0-or-later
use chrono::Local;
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Axis, Block, BorderType, Borders, Chart, Dataset, Gauge, GraphType, Paragraph},
    Frame, Terminal,
};
use std::{
    error::Error,
    fs::File,
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use sysinfo::{Components, System};

/// One-line description shown under the title on the splash screen, in the
/// dashboard header, and in headless mode.
const TAGLINE: &str = "Maximize CPU load, watch the battery drain in real time, \
and measure worst-case battery life.";

/// Command line arguments
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Plot data from a previously recorded CSV log instead of draining
    #[arg(short, long, value_name = "FILE")]
    plot: Option<PathBuf>,

    /// Run without the interactive TUI: stream status to stdout (great for SSH/CI)
    #[arg(short = 'H', long)]
    headless: bool,

    /// Stop automatically after this many minutes (0 = run until you quit)
    #[arg(short, long, value_name = "MINUTES", default_value_t = 0.0)]
    duration: f64,

    /// Number of CPU load threads to spawn (default: one per logical core)
    #[arg(short, long, value_name = "N")]
    threads: Option<usize>,

    /// Monitor the battery only — do not spawn CPU load threads
    #[arg(long)]
    no_load: bool,

    /// Log file path (default: drain_log_<timestamp>.csv)
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,
}

/// A single record for logging
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct LogEntry {
    timestamp: f64,
    percentage: f32,
    drain_rate: f32, // in % per minute
    #[serde(default)]
    cpu_usage: f32, // 0-100%
    #[serde(default)]
    memory_usage: f32, // 0-100%
    #[serde(default)]
    cpu_temp: f32, // Celsius
    #[serde(default)]
    battery_temp: f32, // Celsius
    #[serde(default)]
    clock_time: String, // HH:MM:SS format
}

/// Holds data for plotting
struct PlotData {
    percentages: Vec<(f64, f64)>,
    drain_rates: Vec<(f64, f64)>,
    cpu_usage: Vec<(f64, f64)>,
    memory_usage: Vec<(f64, f64)>,
    cpu_temp: Vec<(f64, f64)>,
    battery_temp: Vec<(f64, f64)>,
    // Original log entries for playback display
    entries: Vec<LogEntry>,
}

/// Playback state for plot mode
struct PlaybackState {
    position: usize,    // Current index in data
    playing: bool,      // Is playback active
    speed: f64,         // Playback speed multiplier (1.0 = real-time)
    last_tick: Instant, // Last update time
    static_view: bool,  // Show all data (static) vs playback view
}

/// App state
struct App {
    start_time: Instant,
    battery_manager: battery::Manager,
    log_writer: csv::Writer<File>,
    data: Vec<LogEntry>,
    log_filename: String,
    system: System,
    components: Components,
}

impl App {
    fn new(output: Option<PathBuf>) -> Result<Self, Box<dyn Error>> {
        let filename = match output {
            Some(path) => path.to_string_lossy().into_owned(),
            None => format!("drain_log_{}.csv", Local::now().format("%Y%m%d_%H%M%S")),
        };
        let log_writer = csv::Writer::from_path(&filename)?;
        println!("Logging data to {}", filename);

        Ok(Self {
            start_time: Instant::now(),
            battery_manager: battery::Manager::new()?,
            log_writer,
            data: Vec::new(),
            log_filename: filename,
            system: System::new_all(),
            components: Components::new_with_refreshed_list(),
        })
    }

    fn on_tick(&mut self) -> Result<(), Box<dyn Error>> {
        // Refresh system info
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.components.refresh();

        // Get CPU usage (average of all cores)
        let cpus = self.system.cpus();
        let cpu_usage = if !cpus.is_empty() {
            cpus.iter().map(|c| c.cpu_usage()).sum::<f32>() / cpus.len() as f32
        } else {
            0.0
        };

        // Get memory usage
        let total_memory = self.system.total_memory() as f64;
        let used_memory = self.system.used_memory() as f64;
        let memory_usage = if total_memory > 0.0 {
            (used_memory / total_memory * 100.0) as f32
        } else {
            0.0
        };

        // Get CPU temperature (look for coretemp, k10temp, or similar)
        let cpu_temp = self
            .components
            .iter()
            .find(|c| {
                let label = c.label().to_lowercase();
                label.contains("core")
                    || label.contains("cpu")
                    || label.contains("k10temp")
                    || label.contains("tctl")
            })
            .map(|c| c.temperature())
            .unwrap_or(0.0);

        // Get current time
        let clock_time = Local::now().format("%H:%M:%S").to_string();

        if let Some(Ok(battery)) = self.battery_manager.batteries()?.next() {
            let percentage = battery.state_of_charge().get::<battery::units::ratio::percent>();
            let elapsed_seconds = self.start_time.elapsed().as_secs_f64();

            // Get battery temperature (if available)
            let battery_temp = battery
                .temperature()
                .map(|t| t.get::<battery::units::thermodynamic_temperature::degree_celsius>())
                .unwrap_or(0.0);

            // Calculate drain rate using a 30-second rolling window for stability
            // Battery percentage typically only updates every 30-60 seconds at OS level
            let drain_rate = if !self.data.is_empty() {
                // Find a sample from ~30 seconds ago (or oldest available)
                let target_time = elapsed_seconds - 30.0;
                let reference_entry = self
                    .data
                    .iter()
                    .rev()
                    .find(|e| e.timestamp <= target_time)
                    .unwrap_or(&self.data[0]);

                let time_diff_secs = elapsed_seconds - reference_entry.timestamp;
                let percent_diff = reference_entry.percentage - percentage;

                if time_diff_secs > 0.5 && percent_diff > 0.0 {
                    // Valid drain measurement
                    (percent_diff as f64 / time_diff_secs * 60.0) as f32
                } else if time_diff_secs > 0.5 {
                    // No drain detected yet, use last known rate if available
                    self.data
                        .iter()
                        .rev()
                        .find(|e| e.drain_rate > 0.01)
                        .map(|e| e.drain_rate)
                        .unwrap_or(0.0)
                } else {
                    0.0
                }
            } else {
                0.0
            };

            let entry = LogEntry {
                timestamp: elapsed_seconds,
                percentage,
                drain_rate,
                cpu_usage,
                memory_usage,
                cpu_temp,
                battery_temp,
                clock_time,
            };

            self.data.push(entry.clone());
            self.log_writer.serialize(entry)?;
            self.log_writer.flush()?;
        }
        Ok(())
    }
}

// ----------------------------------------------------------------------------
// Presentation helpers (pure functions — covered by unit tests below)
// ----------------------------------------------------------------------------

/// A 5-row block-font glyph for the small set of letters our title needs.
fn glyph(c: char) -> [&'static str; 5] {
    match c {
        'B' => ["████ ", "█   █", "████ ", "█   █", "████ "],
        'A' => [" ███ ", "█   █", "█████", "█   █", "█   █"],
        'T' => ["█████", "  █  ", "  █  ", "  █  ", "  █  "],
        'E' => ["█████", "█    ", "████ ", "█    ", "█████"],
        'R' => ["████ ", "█   █", "████ ", "█  █ ", "█   █"],
        'Y' => ["█   █", "█   █", " ███ ", "  █  ", "  █  "],
        'D' => ["████ ", "█   █", "█   █", "█   █", "████ "],
        'I' => ["█████", "  █  ", "  █  ", "  █  ", "█████"],
        'N' => ["█   █", "██  █", "█ █ █", "█  ██", "█   █"],
        _ => ["     ", "     ", "     ", "     ", "     "],
    }
}

/// Render a single word as 5 block-font rows of equal display width.
fn banner_word(word: &str) -> Vec<String> {
    let glyphs: Vec<[&str; 5]> = word.chars().map(glyph).collect();
    (0..5)
        .map(|row| {
            glyphs
                .iter()
                .map(|g| g[row])
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// The full "BATTERY / DRAINER" block-font title, stacked on two lines.
fn banner_lines() -> Vec<String> {
    let mut lines = banner_word("BATTERY");
    lines.push(String::new());
    lines.extend(banner_word("DRAINER"));
    lines
}

/// Format a number of seconds as `1h 02m 05s`.
fn format_hms(total_secs: f64) -> String {
    let secs = total_secs.max(0.0) as u64;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{}h {:02}m {:02}s", h, m, s)
}

/// Mean drain rate over the whole session (matches the live dashboard average).
fn average_drain_rate(data: &[LogEntry]) -> f32 {
    if data.len() > 1 {
        let total: f32 = data.iter().map(|e| e.drain_rate).sum();
        total / data.len() as f32
    } else {
        data.first().map(|e| e.drain_rate).unwrap_or(0.0)
    }
}

/// Estimate the time to fully drain from the current percentage and average rate.
fn estimate_time_remaining(percentage: f32, avg_drain_rate: f32) -> String {
    if avg_drain_rate > 0.01 {
        let minutes_remaining = percentage / avg_drain_rate;
        format_hms(minutes_remaining as f64 * 60.0)
    } else {
        "Calculating...".to_string()
    }
}

/// A rolled-up summary of a drain session, printed when a run ends.
struct DrainSummary {
    samples: usize,
    duration_secs: f64,
    start_pct: f32,
    end_pct: f32,
    drained_pct: f32,
    avg_drain_rate: f32,
    peak_cpu_usage: f32,
    peak_cpu_temp: f32,
    peak_battery_temp: f32,
}

/// Summarize a session's log entries. Returns `None` if nothing was recorded.
fn summarize(data: &[LogEntry]) -> Option<DrainSummary> {
    let first = data.first()?;
    let last = data.last()?;
    Some(DrainSummary {
        samples: data.len(),
        duration_secs: last.timestamp - first.timestamp,
        start_pct: first.percentage,
        end_pct: last.percentage,
        drained_pct: first.percentage - last.percentage,
        avg_drain_rate: average_drain_rate(data),
        peak_cpu_usage: data.iter().map(|e| e.cpu_usage).fold(0.0, f32::max),
        peak_cpu_temp: data.iter().map(|e| e.cpu_temp).fold(0.0, f32::max),
        peak_battery_temp: data.iter().map(|e| e.battery_temp).fold(0.0, f32::max),
    })
}

impl DrainSummary {
    fn render(&self) -> String {
        let batt_temp = if self.peak_battery_temp > 0.0 {
            format!("{:.0}°C", self.peak_battery_temp)
        } else {
            "N/A".to_string()
        };
        format!(
            "==================== Session Summary ====================\n\
             Duration:        {} ({} samples)\n\
             Battery:         {:.1}% -> {:.1}%  (drained {:.1}%)\n\
             Avg drain rate:  {:.2} %/min\n\
             Est. full drain: {}\n\
             Peak CPU usage:  {:.0}%\n\
             Peak CPU temp:   {:.0}°C\n\
             Peak batt temp:  {}\n\
             =========================================================",
            format_hms(self.duration_secs),
            self.samples,
            self.start_pct,
            self.end_pct,
            self.drained_pct,
            self.avg_drain_rate,
            estimate_time_remaining(self.end_pct, self.avg_drain_rate),
            self.peak_cpu_usage,
            self.peak_cpu_temp,
            batt_temp,
        )
    }
}

/// Spawn `count` CPU-burning threads that respect the shared run/active flags.
/// Threads exit when `run` is cleared; they idle when `active` is cleared.
fn spawn_load(count: usize, run: &Arc<AtomicBool>, active: &Arc<AtomicBool>) -> Vec<thread::JoinHandle<()>> {
    (0..count)
        .map(|_| {
            let run = Arc::clone(run);
            let active = Arc::clone(active);
            thread::spawn(move || {
                let mut x = 0.0f64;
                while run.load(Ordering::Relaxed) {
                    if active.load(Ordering::Relaxed) {
                        x = (x + 1.0).sqrt();
                    } else {
                        thread::sleep(Duration::from_millis(100));
                    }
                }
                let _ = x;
            })
        })
        .collect()
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    if let Some(plot_file) = args.plot.clone() {
        run_plot_mode(plot_file)?;
    } else if args.headless {
        run_headless(&args)?;
    } else {
        run_drain_mode(&args)?;
    }

    Ok(())
}

fn run_drain_mode(args: &Args) -> Result<(), Box<dyn Error>> {
    let num_threads = args.threads.unwrap_or_else(num_cpus::get);
    let target = (args.duration > 0.0).then(|| Duration::from_secs_f64(args.duration * 60.0));

    // Start the CPU drainer threads
    let run_flag = Arc::new(AtomicBool::new(true));
    let load_active = Arc::new(AtomicBool::new(!args.no_load));
    let handles = if args.no_load {
        Vec::new()
    } else {
        spawn_load(num_threads, &run_flag, &load_active)
    };

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Splash: big beautiful title + description, dismissible with any key.
    let splash_start = Instant::now();
    loop {
        terminal.draw(ui_splash)?;
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(_) = event::read()? {
                break;
            }
        }
        if splash_start.elapsed() >= Duration::from_millis(2200) {
            break;
        }
    }

    let mut app = App::new(args.output.clone())?;
    let tick_rate = Duration::from_millis(1000);
    let mut last_tick = Instant::now();

    loop {
        let paused = !load_active.load(Ordering::Relaxed);
        let elapsed = app.start_time.elapsed().as_secs_f64();
        terminal.draw(|f| ui_drain(f, &app, paused, args.no_load, elapsed, target))?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    // Pause/resume the CPU load without leaving the dashboard.
                    KeyCode::Char(' ') | KeyCode::Char('p') => {
                        if !args.no_load {
                            let now = load_active.load(Ordering::Relaxed);
                            load_active.store(!now, Ordering::Relaxed);
                        }
                    }
                    _ => {}
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            if let Err(e) = app.on_tick() {
                eprintln!("Error during tick: {}", e);
                break;
            }
            last_tick = Instant::now();
        }

        if let Some(t) = target {
            if app.start_time.elapsed() >= t {
                break;
            }
        }
    }

    // Stop the load threads cleanly.
    run_flag.store(false, Ordering::Relaxed);
    load_active.store(true, Ordering::Relaxed); // wake idling threads so they can exit
    for h in handles {
        let _ = h.join();
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;

    // Print a session summary now that the alternate screen is gone.
    if let Some(summary) = summarize(&app.data) {
        println!("\n{}", summary.render());
        println!("Log saved to {}", app.log_filename);
    }
    Ok(())
}

fn run_headless(args: &Args) -> Result<(), Box<dyn Error>> {
    let num_threads = args.threads.unwrap_or_else(num_cpus::get);
    let target = (args.duration > 0.0).then(|| Duration::from_secs_f64(args.duration * 60.0));

    // Big beautiful title + description, straight to stdout.
    for line in banner_lines() {
        println!("{}", line);
    }
    println!("\n{}\n", TAGLINE);

    let run_flag = Arc::new(AtomicBool::new(true));
    let load_active = Arc::new(AtomicBool::new(!args.no_load));
    let handles = if args.no_load {
        Vec::new()
    } else {
        spawn_load(num_threads, &run_flag, &load_active)
    };

    let mut app = App::new(args.output.clone())?;
    let mode = if args.no_load { "monitor-only" } else { "drain" };
    let dur = match target {
        Some(t) => format_hms(t.as_secs_f64()),
        None => "until Ctrl-C".to_string(),
    };
    let threads_note = if args.no_load {
        "0 (monitoring)".to_string()
    } else {
        num_threads.to_string()
    };
    println!(
        "Mode: {}  |  load threads: {}  |  duration: {}",
        mode, threads_note, dur
    );
    println!("Streaming status every 10s. Press Ctrl-C to stop.\n");

    let tick_rate = Duration::from_millis(1000);
    let status_every = Duration::from_secs(10);
    let mut last_status: Option<Instant> = None;

    loop {
        if let Err(e) = app.on_tick() {
            eprintln!("Error during tick: {}", e);
            break;
        }

        let due = last_status.map(|t| t.elapsed() >= status_every).unwrap_or(true);
        if due {
            print_status_line(&app);
            last_status = Some(Instant::now());
        }

        if let Some(t) = target {
            if app.start_time.elapsed() >= t {
                break;
            }
        }
        thread::sleep(tick_rate);
    }

    run_flag.store(false, Ordering::Relaxed);
    load_active.store(true, Ordering::Relaxed);
    for h in handles {
        let _ = h.join();
    }

    if let Some(summary) = summarize(&app.data) {
        println!("\n{}", summary.render());
        println!("Log saved to {}", app.log_filename);
    }
    Ok(())
}

/// Print one compact status line for headless mode.
fn print_status_line(app: &App) {
    let elapsed = format_hms(app.start_time.elapsed().as_secs_f64());
    if let Some(e) = app.data.last() {
        let avg = average_drain_rate(&app.data);
        let batt_temp = if e.battery_temp > 0.0 {
            format!("{:.0}°C", e.battery_temp)
        } else {
            "N/A".to_string()
        };
        println!(
            "[{}]  Battery {:>5.1}%  Drain {:>5.2}%/min (avg {:>5.2})  CPU {:>3.0}%  Mem {:>3.0}%  CPU {:>3.0}°C  Batt {}",
            elapsed, e.percentage, e.drain_rate, avg, e.cpu_usage, e.memory_usage, e.cpu_temp, batt_temp
        );
    } else {
        println!("[{}]  waiting for first battery reading...", elapsed);
    }
}

/// Full-screen splash: the big block-font title and a one-line description.
fn ui_splash(f: &mut Frame) {
    let area = f.size();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(22), Constraint::Min(0)])
        .split(area);

    let mut lines: Vec<Line> = banner_lines()
        .into_iter()
        .map(|l| {
            Line::from(Span::styled(
                l,
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ))
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        TAGLINE,
        Style::default().fg(Color::Gray),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Press any key to begin  ▸",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));

    let splash = Paragraph::new(lines).alignment(Alignment::Center);
    f.render_widget(splash, layout[1]);
}

/// Compact dashboard header: stylized title + description in a rounded block.
fn header_widget<'a>(paused: bool, monitor_only: bool) -> Paragraph<'a> {
    let mut title_spans = vec![Span::styled(
        "⚡  B A T T E R Y   D R A I N E R  ⚡",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    if monitor_only {
        title_spans.push(Span::styled(
            "   [MONITOR ONLY]",
            Style::default().fg(Color::Blue),
        ));
    } else if paused {
        title_spans.push(Span::styled(
            "   ⏸ LOAD PAUSED",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let lines = vec![
        Line::from(title_spans),
        Line::from(Span::styled(TAGLINE, Style::default().fg(Color::Gray))),
    ];
    Paragraph::new(lines)
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Cyan)),
        )
}

fn ui_drain(
    f: &mut Frame,
    app: &App,
    paused: bool,
    monitor_only: bool,
    elapsed: f64,
    target: Option<Duration>,
) {
    // Layout
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // Header banner + description
            Constraint::Length(1), // For the clock and temps
            Constraint::Length(1), // For drain rate stats
            Constraint::Length(2), // For the battery gauge
            Constraint::Length(2), // For CPU/Memory gauges
            Constraint::Min(0),    // For the chart
            Constraint::Length(1), // For the status line
        ])
        .split(f.size());

    // --- Header ---
    f.render_widget(header_widget(paused, monitor_only), layout[0]);

    // Get current values
    let (current_percentage, drain_rate, cpu_usage, memory_usage, cpu_temp, battery_temp, clock_time) =
        app.data
            .last()
            .map(|entry| {
                (
                    entry.percentage,
                    entry.drain_rate,
                    entry.cpu_usage,
                    entry.memory_usage,
                    entry.cpu_temp,
                    entry.battery_temp,
                    entry.clock_time.clone(),
                )
            })
            .unwrap_or((
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                Local::now().format("%H:%M:%S").to_string(),
            ));

    // Calculate average drain rate over the session
    let avg_drain_rate = average_drain_rate(&app.data);

    // Calculate estimated time remaining
    let time_remaining = estimate_time_remaining(current_percentage, avg_drain_rate);

    // --- Clock and Temperature Line ---
    let elapsed_str = match target {
        Some(t) => format!("{} / {}", format_hms(elapsed), format_hms(t.as_secs_f64())),
        None => format_hms(elapsed),
    };
    let clock_spans = vec![
        Span::styled("Elapsed: ", Style::default().fg(Color::White)),
        Span::styled(
            elapsed_str,
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  |  "),
        Span::styled("Time: ", Style::default().fg(Color::White)),
        Span::styled(&clock_time, Style::default().fg(Color::Cyan)),
        Span::raw("  |  "),
        Span::styled("CPU Temp: ", Style::default().fg(Color::White)),
        Span::styled(
            format!("{:.1}°C", cpu_temp),
            Style::default().fg(if cpu_temp > 80.0 {
                Color::Red
            } else if cpu_temp > 60.0 {
                Color::Yellow
            } else {
                Color::Green
            }),
        ),
        Span::raw("  |  "),
        Span::styled("Battery Temp: ", Style::default().fg(Color::White)),
        Span::styled(
            if battery_temp > 0.0 {
                format!("{:.1}°C", battery_temp)
            } else {
                "N/A".to_string()
            },
            Style::default().fg(if battery_temp > 45.0 {
                Color::Red
            } else if battery_temp > 35.0 {
                Color::Yellow
            } else {
                Color::Green
            }),
        ),
    ];
    let clock_line = Paragraph::new(Line::from(clock_spans));
    f.render_widget(clock_line, layout[1]);

    // --- Drain Rate Stats Line ---
    let drain_spans = vec![
        Span::styled("Drain Rate: ", Style::default().fg(Color::White)),
        Span::styled(
            format!("{:.2}%/min", drain_rate),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  |  "),
        Span::styled("Avg: ", Style::default().fg(Color::White)),
        Span::styled(
            format!("{:.2}%/min", avg_drain_rate),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw("  |  "),
        Span::styled("Est. Remaining: ", Style::default().fg(Color::White)),
        Span::styled(
            time_remaining,
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ),
    ];
    let drain_line = Paragraph::new(Line::from(drain_spans));
    f.render_widget(drain_line, layout[2]);

    // --- Battery Gauge ---
    let battery_color = if current_percentage > 75.0 {
        Color::Green
    } else if current_percentage > 50.0 {
        Color::Yellow
    } else if current_percentage > 25.0 {
        Color::Rgb(255, 165, 0)
    } else {
        Color::Red
    };
    let battery_gauge = Gauge::default()
        .block(Block::default().title("Battery"))
        .gauge_style(
            Style::default()
                .fg(battery_color)
                .bg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .ratio(current_percentage as f64 / 100.0)
        .label(format!("{:.1}%", current_percentage));
    f.render_widget(battery_gauge, layout[3]);

    // --- CPU and Memory Gauges (side by side) ---
    let system_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(layout[4]);

    let cpu_gauge = Gauge::default()
        .block(Block::default().title("CPU"))
        .gauge_style(
            Style::default()
                .fg(Color::Blue)
                .bg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .ratio((cpu_usage as f64 / 100.0).min(1.0))
        .label(format!("{:.1}%", cpu_usage));
    f.render_widget(cpu_gauge, system_layout[0]);

    let memory_gauge = Gauge::default()
        .block(Block::default().title("Memory"))
        .gauge_style(
            Style::default()
                .fg(Color::Magenta)
                .bg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .ratio((memory_usage as f64 / 100.0).min(1.0))
        .label(format!("{:.1}%", memory_usage));
    f.render_widget(memory_gauge, system_layout[1]);

    // --- Chart ---
    let mut percentages_data = Vec::new();
    let mut drain_rates_data = Vec::new();
    let mut cpu_data = Vec::new();
    let mut memory_data = Vec::new();
    for entry in &app.data {
        percentages_data.push((entry.timestamp, entry.percentage as f64));
        drain_rates_data.push((entry.timestamp, entry.drain_rate as f64));
        cpu_data.push((entry.timestamp, entry.cpu_usage as f64));
        memory_data.push((entry.timestamp, entry.memory_usage as f64));
    }
    let (x_min, x_max) = percentages_data
        .first()
        .zip(percentages_data.last())
        .map(|(first, last)| (first.0, last.0.max(first.0 + 1.0)))
        .unwrap_or((0.0, 60.0));

    let max_drain_rate = drain_rates_data
        .iter()
        .map(|&(_, val)| val)
        .fold(0.0, f64::max)
        .max(1.0);

    let normalized_drain_rates: Vec<(f64, f64)> = drain_rates_data
        .iter()
        .map(|(x, y)| (*x, (y / max_drain_rate) * 100.0))
        .collect();

    let datasets = vec![
        Dataset::default()
            .name("Battery %")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Green))
            .data(&percentages_data),
        Dataset::default()
            .name("Drain Rate")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Red))
            .data(&normalized_drain_rates),
        Dataset::default()
            .name("CPU %")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Blue))
            .data(&cpu_data),
        Dataset::default()
            .name("Memory %")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Magenta))
            .data(&memory_data),
    ];

    // Generate Y-axis labels (0%, 5%, 10%, ..., 100%)
    let y_labels: Vec<Span> = (0..=20).map(|i| Span::raw(format!("{:3}%", i * 5))).collect();

    // Generate X-axis labels based on time range
    let x_range = x_max - x_min;
    let x_step = if x_range <= 60.0 {
        10.0
    } else if x_range <= 300.0 {
        30.0
    } else {
        60.0
    };
    let x_labels: Vec<Span> = {
        let mut labels = Vec::new();
        let mut t = (x_min / x_step).floor() * x_step;
        while t <= x_max {
            if t >= x_min {
                labels.push(Span::raw(format!("{:.0}s", t)));
            }
            t += x_step;
        }
        if labels.is_empty() {
            labels.push(Span::raw(format!("{:.0}s", x_min)));
            labels.push(Span::raw(format!("{:.0}s", x_max)));
        }
        labels
    };

    let chart = Chart::new(datasets)
        .block(Block::default().title("Real-time Analysis").borders(Borders::ALL))
        .x_axis(
            Axis::default()
                .title("Time (s)")
                .style(Style::default().fg(Color::Gray))
                .bounds([x_min, x_max])
                .labels(x_labels),
        )
        .y_axis(
            Axis::default()
                .title("%")
                .style(Style::default().fg(Color::Yellow))
                .bounds([0.0, 100.0])
                .labels(y_labels),
        );
    f.render_widget(chart, layout[5]);

    // --- Status / Controls Line ---
    let load_hint: Vec<Span> = if monitor_only {
        vec![]
    } else {
        vec![
            Span::styled("space/p", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(":pause load  "),
        ]
    };
    let mut status_spans = vec![
        Span::styled("q", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(":quit  "),
    ];
    status_spans.extend(load_hint);
    status_spans.push(Span::raw("|| "));
    status_spans.push(Span::styled("B%", Style::default().fg(Color::Green)));
    status_spans.push(Span::raw(" "));
    status_spans.push(Span::styled("DR", Style::default().fg(Color::Red)));
    status_spans.push(Span::raw(" "));
    status_spans.push(Span::styled("CPU", Style::default().fg(Color::Blue)));
    status_spans.push(Span::raw(" "));
    status_spans.push(Span::styled("MEM", Style::default().fg(Color::Magenta)));
    status_spans.push(Span::raw(" || "));
    status_spans.push(Span::styled(
        &app.log_filename,
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let status_line = Paragraph::new(Line::from(status_spans));
    f.render_widget(status_line, layout[6]);
}

fn run_plot_mode(path: PathBuf) -> Result<(), Box<dyn Error>> {
    let mut rdr = csv::Reader::from_path(path)?;
    let mut data = PlotData {
        percentages: Vec::new(),
        drain_rates: Vec::new(),
        cpu_usage: Vec::new(),
        memory_usage: Vec::new(),
        cpu_temp: Vec::new(),
        battery_temp: Vec::new(),
        entries: Vec::new(),
    };
    for result in rdr.deserialize() {
        let record: LogEntry = result?;
        data.percentages.push((record.timestamp, record.percentage as f64));
        data.drain_rates.push((record.timestamp, record.drain_rate as f64));
        data.cpu_usage.push((record.timestamp, record.cpu_usage as f64));
        data.memory_usage.push((record.timestamp, record.memory_usage as f64));
        data.cpu_temp.push((record.timestamp, record.cpu_temp as f64));
        data.battery_temp.push((record.timestamp, record.battery_temp as f64));
        data.entries.push(record);
    }

    if data.percentages.is_empty() {
        println!("No data to plot.");
        return Ok(());
    }

    // Initialize playback state
    let mut playback = PlaybackState {
        position: 0,
        playing: false,
        speed: 1.0,
        last_tick: Instant::now(),
        static_view: true, // Start in static view
    };

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    loop {
        terminal.draw(|f| ui_plot(f, &data, &playback))?;

        // Handle playback advancement
        if playback.playing && !playback.static_view && playback.position < data.entries.len() - 1 {
            let elapsed = playback.last_tick.elapsed().as_secs_f64();
            let current_time = data.entries[playback.position].timestamp;
            let next_time = data.entries[playback.position + 1].timestamp;
            let time_diff = next_time - current_time;

            // Advance if enough real time has passed (adjusted by speed)
            if elapsed >= time_diff / playback.speed {
                playback.position += 1;
                playback.last_tick = Instant::now();
            }
        }

        if crossterm::event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    // Toggle static/playback view
                    KeyCode::Char('v') => {
                        playback.static_view = !playback.static_view;
                        if !playback.static_view {
                            playback.position = 0;
                            playback.last_tick = Instant::now();
                        }
                    }
                    // Play/Pause
                    KeyCode::Char(' ') => {
                        if !playback.static_view {
                            playback.playing = !playback.playing;
                            playback.last_tick = Instant::now();
                        }
                    }
                    // Speed up
                    KeyCode::Char('+') | KeyCode::Char('=') => {
                        playback.speed = (playback.speed * 2.0).min(64.0);
                    }
                    // Slow down
                    KeyCode::Char('-') => {
                        playback.speed = (playback.speed / 2.0).max(0.25);
                    }
                    // Rewind (go back 10 samples)
                    KeyCode::Left => {
                        if !playback.static_view {
                            playback.position = playback.position.saturating_sub(10);
                            playback.last_tick = Instant::now();
                        }
                    }
                    // Fast forward (skip ahead 10 samples)
                    KeyCode::Right => {
                        if !playback.static_view {
                            playback.position = (playback.position + 10).min(data.entries.len() - 1);
                            playback.last_tick = Instant::now();
                        }
                    }
                    // Jump to start
                    KeyCode::Home => {
                        if !playback.static_view {
                            playback.position = 0;
                            playback.last_tick = Instant::now();
                        }
                    }
                    // Jump to end
                    KeyCode::End => {
                        if !playback.static_view {
                            playback.position = data.entries.len() - 1;
                            playback.last_tick = Instant::now();
                        }
                    }
                    // Reset speed to 1x
                    KeyCode::Char('r') => {
                        playback.speed = 1.0;
                    }
                    _ => {}
                }
            }
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

fn ui_plot(f: &mut Frame, data: &PlotData, playback: &PlaybackState) {
    // Layout with info bars
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Playback info
            Constraint::Length(1), // Current values (in playback mode)
            Constraint::Min(0),    // Chart
            Constraint::Length(1), // Controls help
        ])
        .split(f.size());

    // Determine data range based on mode
    let display_end = if playback.static_view {
        data.percentages.len()
    } else {
        playback.position + 1
    };

    let percentages_slice = &data.percentages[..display_end];
    let drain_rates_slice = &data.drain_rates[..display_end];
    let cpu_usage_slice = &data.cpu_usage[..display_end];
    let memory_usage_slice = &data.memory_usage[..display_end];

    let (x_min, x_max) = if playback.static_view {
        // Static view: show full range
        data.percentages
            .first()
            .zip(data.percentages.last())
            .map(|(first, last)| (first.0, last.0.max(first.0 + 1.0)))
            .unwrap_or((0.0, 60.0))
    } else {
        // Playback view: start from 0, end at current position
        let end_time = data
            .percentages
            .get(playback.position)
            .map(|(t, _)| *t)
            .unwrap_or(60.0);
        (0.0, end_time.max(1.0))
    };

    let max_drain_rate = data
        .drain_rates
        .iter()
        .map(|&(_, val)| val)
        .fold(0.0, f64::max)
        .max(1.0);

    let normalized_drain_rates: Vec<(f64, f64)> = drain_rates_slice
        .iter()
        .map(|(x, y)| (*x, (y / max_drain_rate) * 100.0))
        .collect();

    // --- Playback Status Line ---
    let mode_str = if playback.static_view {
        "STATIC VIEW"
    } else {
        "PLAYBACK"
    };
    let play_status = if playback.static_view {
        "".to_string()
    } else if playback.playing {
        "▶ Playing".to_string()
    } else {
        "⏸ Paused".to_string()
    };
    let position_str = if playback.static_view {
        format!("Total: {} samples", data.entries.len())
    } else {
        format!("{}/{}", playback.position + 1, data.entries.len())
    };
    let speed_str = format!("Speed: {:.2}x", playback.speed);

    let status_spans = vec![
        Span::styled(
            mode_str,
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  |  "),
        Span::styled(
            play_status,
            Style::default().fg(if playback.playing {
                Color::Green
            } else {
                Color::Yellow
            }),
        ),
        Span::raw("  |  "),
        Span::styled(position_str, Style::default().fg(Color::White)),
        Span::raw("  |  "),
        Span::styled(speed_str, Style::default().fg(Color::Magenta)),
    ];
    let status_line = Paragraph::new(Line::from(status_spans));
    f.render_widget(status_line, layout[0]);

    // --- Current Values Line (in playback mode) ---
    let values_line = if !playback.static_view && !data.entries.is_empty() {
        let entry = &data.entries[playback.position];
        let spans = vec![
            Span::styled("Time: ", Style::default().fg(Color::White)),
            Span::styled(format!("{:.0}s", entry.timestamp), Style::default().fg(Color::Cyan)),
            Span::raw("  |  "),
            Span::styled("Battery: ", Style::default().fg(Color::White)),
            Span::styled(format!("{:.1}%", entry.percentage), Style::default().fg(Color::Green)),
            Span::raw("  |  "),
            Span::styled("Drain: ", Style::default().fg(Color::White)),
            Span::styled(format!("{:.2}%/min", entry.drain_rate), Style::default().fg(Color::Red)),
            Span::raw("  |  "),
            Span::styled("CPU: ", Style::default().fg(Color::White)),
            Span::styled(format!("{:.1}%", entry.cpu_usage), Style::default().fg(Color::Blue)),
            Span::raw("  |  "),
            Span::styled("Mem: ", Style::default().fg(Color::White)),
            Span::styled(format!("{:.1}%", entry.memory_usage), Style::default().fg(Color::Magenta)),
            Span::raw("  |  "),
            Span::styled("CPU°: ", Style::default().fg(Color::White)),
            Span::styled(format!("{:.1}°C", entry.cpu_temp), Style::default().fg(Color::Yellow)),
        ];
        Paragraph::new(Line::from(spans))
    } else {
        Paragraph::new(Line::from(vec![Span::styled(
            "Press 'v' to toggle playback mode",
            Style::default().fg(Color::Gray),
        )]))
    };
    f.render_widget(values_line, layout[1]);

    // --- Chart ---
    let datasets = vec![
        Dataset::default()
            .name("Battery %")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Green))
            .data(percentages_slice),
        Dataset::default()
            .name("Drain Rate")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Red))
            .data(&normalized_drain_rates),
        Dataset::default()
            .name("CPU %")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Blue))
            .data(cpu_usage_slice),
        Dataset::default()
            .name("Memory %")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Magenta))
            .data(memory_usage_slice),
    ];

    // Generate Y-axis labels (0%, 5%, 10%, ..., 100%)
    let y_labels: Vec<Span> = (0..=20).map(|i| Span::raw(format!("{:3}%", i * 5))).collect();

    // Generate X-axis labels based on time range
    let x_range = x_max - x_min;
    let x_step = if x_range <= 60.0 {
        10.0
    } else if x_range <= 300.0 {
        30.0
    } else {
        60.0
    };
    let x_labels: Vec<Span> = {
        let mut labels = Vec::new();
        let mut t = (x_min / x_step).floor() * x_step;
        while t <= x_max {
            if t >= x_min {
                labels.push(Span::raw(format!("{:.0}s", t)));
            }
            t += x_step;
        }
        if labels.is_empty() {
            labels.push(Span::raw(format!("{:.0}s", x_min)));
            labels.push(Span::raw(format!("{:.0}s", x_max)));
        }
        labels
    };

    let title = if playback.static_view {
        "Historical Data (Static)"
    } else {
        "Historical Data (Playback)"
    };
    let chart = Chart::new(datasets)
        .block(Block::default().title(title).borders(Borders::ALL))
        .x_axis(
            Axis::default()
                .title("Time (s)")
                .style(Style::default().fg(Color::Gray))
                .bounds([x_min, x_max])
                .labels(x_labels),
        )
        .y_axis(
            Axis::default()
                .title("%")
                .style(Style::default().fg(Color::Yellow))
                .bounds([0.0, 100.0])
                .labels(y_labels),
        );
    f.render_widget(chart, layout[2]);

    // --- Controls Help Line ---
    let controls = vec![
        Span::styled("v", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(":view "),
        Span::styled("Space", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(":play/pause "),
        Span::styled("+/-", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(":speed "),
        Span::styled("←/→", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(":skip "),
        Span::styled("Home/End", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(":jump "),
        Span::styled("r", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(":reset speed "),
        Span::styled("q", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(":quit"),
    ];
    let controls_line = Paragraph::new(Line::from(controls));
    f.render_widget(controls_line, layout[3]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(timestamp: f64, percentage: f32, drain_rate: f32) -> LogEntry {
        LogEntry {
            timestamp,
            percentage,
            drain_rate,
            cpu_usage: 90.0,
            memory_usage: 40.0,
            cpu_temp: 70.0,
            battery_temp: 35.0,
            clock_time: "12:00:00".to_string(),
        }
    }

    #[test]
    fn banner_word_rows_are_aligned() {
        let rows = banner_word("BATTERY");
        assert_eq!(rows.len(), 5);
        let width = rows[0].chars().count();
        assert!(width > 0);
        for row in &rows {
            assert_eq!(row.chars().count(), width, "row `{}` has wrong width", row);
        }
    }

    #[test]
    fn banner_lines_has_both_words() {
        let lines = banner_lines();
        // 5 rows per word plus one blank separator line.
        assert_eq!(lines.len(), 11);
        assert!(lines[5].is_empty());
    }

    #[test]
    fn format_hms_formats_hours_minutes_seconds() {
        assert_eq!(format_hms(0.0), "0h 00m 00s");
        assert_eq!(format_hms(65.0), "0h 01m 05s");
        assert_eq!(format_hms(3725.0), "1h 02m 05s");
        // Negative durations clamp to zero rather than panic.
        assert_eq!(format_hms(-10.0), "0h 00m 00s");
    }

    #[test]
    fn average_drain_rate_is_the_mean() {
        let data = vec![
            entry(0.0, 100.0, 1.0),
            entry(1.0, 99.0, 2.0),
            entry(2.0, 98.0, 3.0),
        ];
        assert!((average_drain_rate(&data) - 2.0).abs() < 1e-6);
        assert_eq!(average_drain_rate(&[]), 0.0);
    }

    #[test]
    fn estimate_time_remaining_handles_no_drain() {
        assert_eq!(estimate_time_remaining(50.0, 0.0), "Calculating...");
        // 50% at 2%/min = 25 minutes.
        assert_eq!(estimate_time_remaining(50.0, 2.0), "0h 25m 00s");
    }

    #[test]
    fn summarize_reports_drain_and_peaks() {
        let data = vec![
            entry(0.0, 90.0, 0.0),
            entry(60.0, 88.0, 2.0),
            entry(120.0, 86.0, 2.0),
        ];
        let s = summarize(&data).expect("summary");
        assert_eq!(s.samples, 3);
        assert!((s.duration_secs - 120.0).abs() < 1e-6);
        assert!((s.start_pct - 90.0).abs() < 1e-6);
        assert!((s.end_pct - 86.0).abs() < 1e-6);
        assert!((s.drained_pct - 4.0).abs() < 1e-6);
        assert!((s.peak_cpu_usage - 90.0).abs() < 1e-6);
        // render() should mention the drained amount.
        assert!(s.render().contains("drained 4.0%"));
    }

    #[test]
    fn summarize_empty_is_none() {
        assert!(summarize(&[]).is_none());
    }
}
