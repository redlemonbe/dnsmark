use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Gauge, Paragraph, Sparkline},
    Terminal,
};

use crate::config::Config;
use crate::stats::StatsCollector;

pub async fn run_tui(
    stats: Arc<StatsCollector>,
    shutdown: Arc<AtomicBool>,
    config: Arc<Config>,
) {
    if let Err(e) = tui_loop(stats, shutdown.clone(), config).await {
        tracing::debug!("TUI error: {}", e);
        shutdown.store(true, Ordering::Relaxed);
    }
}

async fn tui_loop(
    stats: Arc<StatsCollector>,
    shutdown: Arc<AtomicBool>,
    config: Arc<Config>,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let start = std::time::Instant::now();
    let mut qps_history: Vec<u64> = Vec::with_capacity(60);
    let mut last_completed = 0u64;
    let mut paused = false;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // Collect stats snapshot
        let elapsed = start.elapsed().as_secs_f64();
        let sent = stats.sent.load(Ordering::Relaxed);
        let completed = stats.completed.load(Ordering::Relaxed);
        let timeouts = stats.timeouts.load(Ordering::Relaxed);
        let errors = stats.errors.load(Ordering::Relaxed);
        let noerror = stats.rcode_noerror.load(Ordering::Relaxed);
        let nxdomain = stats.rcode_nxdomain.load(Ordering::Relaxed);
        let servfail = stats.rcode_servfail.load(Ordering::Relaxed);
        let refused = stats.rcode_refused.load(Ordering::Relaxed);
        let snap = stats.snapshot(elapsed);

        let delta = completed.saturating_sub(last_completed);
        last_completed = completed;
        if qps_history.len() >= 60 {
            qps_history.remove(0);
        }
        qps_history.push(delta);
        let current_qps = delta;
        let peak_qps = *qps_history.iter().max().unwrap_or(&0);

        let progress = if config.duration_secs > 0 && !config.ramp {
            ((elapsed / config.duration_secs as f64) * 100.0).min(100.0) as u16
        } else {
            0
        };

        let server_str = format!(
            "{}:{} — {}",
            config.server, config.port,
            config.protocol.as_str()
        );
        let time_str = if config.ramp {
            format!("{:.1}s", elapsed)
        } else {
            format!("{:.1}s / {}s", elapsed, config.duration_secs)
        };
        let title = format!(" dnsmark 0.1.0 — {} — {} ", server_str, time_str);

        terminal.draw(|f| {
            let area = f.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3), // title + progress
                    Constraint::Length(3), // QPS sparkline
                    Constraint::Length(3), // latency
                    Constraint::Length(4), // counters
                    Constraint::Length(1), // footer
                ])
                .split(area);

            // Progress bar
            let gauge = Gauge::default()
                .block(Block::default().title(title.as_str()).borders(Borders::ALL))
                .gauge_style(Style::default().fg(Color::Green))
                .percent(progress);
            f.render_widget(gauge, chunks[0]);

            // QPS sparkline
            let qps_title = format!(
                " QPS (1s window) — current: {}  target: {}  peak: {} ",
                current_qps,
                if config.qps > 0 { config.qps.to_string() } else { "∞".to_string() },
                peak_qps
            );
            let sparkline = Sparkline::default()
                .block(Block::default().title(qps_title.as_str()).borders(Borders::ALL))
                .data(&qps_history)
                .style(Style::default().fg(Color::Yellow));
            f.render_widget(sparkline, chunks[1]);

            // Latency
            let lat_text = format!(
                "  p50: {:.1}ms    p95: {:.1}ms    p99: {:.1}ms    p999: {:.1}ms",
                snap.p50_us as f64 / 1000.0,
                snap.p95_us as f64 / 1000.0,
                snap.p99_us as f64 / 1000.0,
                snap.p999_us as f64 / 1000.0,
            );
            let lat = Paragraph::new(lat_text)
                .block(Block::default().title(" Latency ").borders(Borders::ALL));
            f.render_widget(lat, chunks[2]);

            // Counters
            let cnt_text = format!(
                "  Sent: {:>10}   Recv: {:>10}   Timeout: {:>8}   Error: {:>8}\n  NOERROR: {:>8}   NXDOMAIN: {:>8}   SERVFAIL: {:>4}   REFUSED: {:>4}",
                sent, completed, timeouts, errors,
                noerror, nxdomain, servfail, refused
            );
            let cnt = Paragraph::new(cnt_text)
                .block(Block::default().title(" Counters ").borders(Borders::ALL));
            f.render_widget(cnt, chunks[3]);

            // Footer
            let footer_text = if paused {
                "  [q] quit   [p] resume   [r] reset stats   — PAUSED"
            } else {
                "  [q] quit   [p] pause    [r] reset stats"
            };
            let footer = Paragraph::new(footer_text)
                .style(Style::default().fg(Color::DarkGray));
            f.render_widget(footer, chunks[4]);
        })?;

        // Handle keyboard events (non-blocking)
        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => {
                        shutdown.store(true, Ordering::Relaxed);
                        break;
                    }
                    KeyCode::Char('p') | KeyCode::Char('P') => {
                        paused = !paused;
                    }
                    KeyCode::Char('r') | KeyCode::Char('R') => {
                        last_completed = completed;
                        qps_history.clear();
                    }
                    _ => {}
                }
            }
        }

        if paused {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
