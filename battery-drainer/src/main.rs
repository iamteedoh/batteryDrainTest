use chrono::Local;
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Axis, Block, Borders, Chart, Dataset, Gauge, GraphType, Paragraph},
    Frame, Terminal,
};
use std::{
    error::Error,
    fs::File,
    io,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use sysinfo::{Components, System};

/// Command line arguments
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Plot data from a log file instead of draining the battery
    #[arg(short, long, value_name = "FILE")]
    plot: Option<PathBuf>,
}

/// A single record for logging
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct LogEntry {
    timestamp: f64,
    percentage: f32,
    drain_rate: f32, // in % per minute
    #[serde(default)]
    cpu_usage: f32,  // 0-100%
    #[serde(default)]
    memory_usage: f32, // 0-100%
    #[serde(default)]
    cpu_temp: f32,   // Celsius
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
    position: usize,       // Current index in data
    playing: bool,         // Is playback active
    speed: f64,            // Playback speed multiplier (1.0 = real-time)
    last_tick: Instant,    // Last update time
    static_view: bool,     // Show all data (static) vs playback view
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
    fn new() -> Result<Self, Box<dyn Error>> {
        let filename = format!("drain_log_{}.csv", Local::now().format("%Y%m%d_%H%M%S"));
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
        let cpu_temp = self.components.iter()
            .find(|c| {
                let label = c.label().to_lowercase();
                label.contains("core") || label.contains("cpu") || label.contains("k10temp") || label.contains("tctl")
            })
            .map(|c| c.temperature())
            .unwrap_or(0.0);

        // Get current time
        let clock_time = Local::now().format("%H:%M:%S").to_string();

        if let Some(Ok(battery)) = self.battery_manager.batteries()?.next() {
            let percentage = battery.state_of_charge().get::<battery::units::ratio::percent>();
            let elapsed_seconds = self.start_time.elapsed().as_secs_f64();

            // Get battery temperature (if available)
            let battery_temp = battery.temperature()
                .map(|t| t.get::<battery::units::thermodynamic_temperature::degree_celsius>())
                .unwrap_or(0.0);

            // Calculate drain rate using a 30-second rolling window for stability
            // Battery percentage typically only updates every 30-60 seconds at OS level
            let drain_rate = if !self.data.is_empty() {
                // Find a sample from ~30 seconds ago (or oldest available)
                let target_time = elapsed_seconds - 30.0;
                let reference_entry = self.data.iter()
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
                    self.data.iter().rev()
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

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    if let Some(plot_file) = args.plot {
        run_plot_mode(plot_file)?;
    } else {
        run_drain_mode()?;
    }

    Ok(())
}

fn run_drain_mode() -> Result<(), Box<dyn Error>> {
    // Start the CPU drainer threads
    let num_threads = num_cpus::get();
    println!("Starting {} CPU-intensive threads.", num_threads);
    for _ in 0..num_threads {
        thread::spawn(move || {
            let mut x = 0.0f64;
            loop {
                x = (x + 1.0).sqrt();
            }
        });
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new()?;
    let tick_rate = Duration::from_millis(1000);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui_drain(f, &app))?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') {
                    break;
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            if let Err(e) = app.on_tick() {
                // Not much we can do in a TUI, maybe log to a file if we had a separate logger
                // For now, we just break the loop
                eprintln!("Error during tick: {}", e);
                break;
            }
            last_tick = Instant::now();
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
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
        data.percentages
            .push((record.timestamp, record.percentage as f64));
        data.drain_rates
            .push((record.timestamp, record.drain_rate as f64));
        data.cpu_usage
            .push((record.timestamp, record.cpu_usage as f64));
        data.memory_usage
            .push((record.timestamp, record.memory_usage as f64));
        data.cpu_temp
            .push((record.timestamp, record.cpu_temp as f64));
        data.battery_temp
            .push((record.timestamp, record.battery_temp as f64));
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
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn ui_drain(f: &mut Frame, app: &App) {
    // Layout
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // For the clock and temps
            Constraint::Length(1), // For drain rate stats
            Constraint::Length(2), // For the battery gauge
            Constraint::Length(2), // For CPU/Memory gauges
            Constraint::Min(0),    // For the chart
            Constraint::Length(1), // For the status line
        ])
        .split(f.size());

    // Get current values
    let (current_percentage, drain_rate, cpu_usage, memory_usage, cpu_temp, battery_temp, clock_time) = app
        .data
        .last()
        .map(|entry| (
            entry.percentage,
            entry.drain_rate,
            entry.cpu_usage,
            entry.memory_usage,
            entry.cpu_temp,
            entry.battery_temp,
            entry.clock_time.clone()
        ))
        .unwrap_or((0.0, 0.0, 0.0, 0.0, 0.0, 0.0, Local::now().format("%H:%M:%S").to_string()));

    // Calculate average drain rate over the session
    let avg_drain_rate = if app.data.len() > 1 {
        let total_drain: f32 = app.data.iter().map(|e| e.drain_rate).sum();
        total_drain / app.data.len() as f32
    } else {
        drain_rate
    };

    // Calculate estimated time remaining
    let time_remaining = if avg_drain_rate > 0.01 {
        let minutes_remaining = current_percentage / avg_drain_rate;
        let hours = (minutes_remaining / 60.0) as u32;
        let mins = (minutes_remaining % 60.0) as u32;
        format!("{}h {:02}m", hours, mins)
    } else {
        "Calculating...".to_string()
    };

    // --- Clock and Temperature Line ---
    let clock_spans = vec![
        Span::styled("Time: ", Style::default().fg(Color::White)),
        Span::styled(&clock_time, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw("  |  "),
        Span::styled("CPU Temp: ", Style::default().fg(Color::White)),
        Span::styled(
            format!("{:.1}°C", cpu_temp),
            Style::default().fg(if cpu_temp > 80.0 { Color::Red } else if cpu_temp > 60.0 { Color::Yellow } else { Color::Green })
        ),
        Span::raw("  |  "),
        Span::styled("Battery Temp: ", Style::default().fg(Color::White)),
        Span::styled(
            if battery_temp > 0.0 { format!("{:.1}°C", battery_temp) } else { "N/A".to_string() },
            Style::default().fg(if battery_temp > 45.0 { Color::Red } else if battery_temp > 35.0 { Color::Yellow } else { Color::Green })
        ),
    ];
    let clock_line = Paragraph::new(Line::from(clock_spans));
    f.render_widget(clock_line, layout[0]);

    // --- Drain Rate Stats Line ---
    let drain_spans = vec![
        Span::styled("Drain Rate: ", Style::default().fg(Color::White)),
        Span::styled(
            format!("{:.2}%/min", drain_rate),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        ),
        Span::raw("  |  "),
        Span::styled("Avg: ", Style::default().fg(Color::White)),
        Span::styled(
            format!("{:.2}%/min", avg_drain_rate),
            Style::default().fg(Color::Yellow)
        ),
        Span::raw("  |  "),
        Span::styled("Est. Remaining: ", Style::default().fg(Color::White)),
        Span::styled(
            time_remaining,
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
        ),
    ];
    let drain_line = Paragraph::new(Line::from(drain_spans));
    f.render_widget(drain_line, layout[1]);

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
    f.render_widget(battery_gauge, layout[2]);

    // --- CPU and Memory Gauges (side by side) ---
    let system_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(layout[3]);

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
    let y_labels: Vec<Span> = (0..=20)
        .map(|i| Span::raw(format!("{:3}%", i * 5)))
        .collect();

    // Generate X-axis labels based on time range
    let x_range = x_max - x_min;
    let x_step = if x_range <= 60.0 { 10.0 } else if x_range <= 300.0 { 30.0 } else { 60.0 };
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
        .block(
            Block::default()
                .title("Real-time Analysis")
                .borders(Borders::ALL),
        )
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
    f.render_widget(chart, layout[4]);

    // --- Status Line ---
    let status_spans = vec![
        Span::styled("B%", Style::default().fg(Color::Green)),
        Span::raw(" | "),
        Span::styled("DR", Style::default().fg(Color::Red)),
        Span::raw(" | "),
        Span::styled("CPU", Style::default().fg(Color::Blue)),
        Span::raw(" | "),
        Span::styled("MEM", Style::default().fg(Color::Magenta)),
        Span::raw(" || Logging to: "),
        Span::styled(
            &app.log_filename,
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(" (q to quit)"),
    ];
    let status_line = Paragraph::new(Line::from(status_spans));
    f.render_widget(status_line, layout[5]);
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
        let end_time = data.percentages.get(playback.position)
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
    let mode_str = if playback.static_view { "STATIC VIEW" } else { "PLAYBACK" };
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
        Span::styled(mode_str, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw("  |  "),
        Span::styled(play_status, Style::default().fg(if playback.playing { Color::Green } else { Color::Yellow })),
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
        Paragraph::new(Line::from(vec![
            Span::styled("Press 'v' to toggle playback mode", Style::default().fg(Color::Gray)),
        ]))
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
    let y_labels: Vec<Span> = (0..=20)
        .map(|i| Span::raw(format!("{:3}%", i * 5)))
        .collect();

    // Generate X-axis labels based on time range
    let x_range = x_max - x_min;
    let x_step = if x_range <= 60.0 { 10.0 } else if x_range <= 300.0 { 30.0 } else { 60.0 };
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

    let title = if playback.static_view { "Historical Data (Static)" } else { "Historical Data (Playback)" };
    let chart = Chart::new(datasets)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL),
        )
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