//! TUI dashboard for the job queue.
//!
//! Renders a live-updating terminal dashboard showing queue status,
//! worker counts, throughput metrics, and recent activity.
//! Keyboard controls allow adding jobs, workers, and quitting.

use crate::job::{Job, random_filename, random_job_type};
use crate::queue::{JobQueue, QueueCall, QueueCast, QueueReply, QueueStatus};
use crate::stats::{StatsCall, StatsCollector, StatsReply, StatsSnapshot};
use crossterm::{
    event::{Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, Paragraph},
};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::dead_letter::{DeadLetterCall, DeadLetterReply, DeadLetterStore};

/// Run the TUI dashboard. This function takes ownership of the terminal
/// and returns when the user quits.
///
/// Must be called from within an ambitious process context (uses gen_server::call).
pub async fn run_tui(job_id_counter: &'static AtomicU64) -> Result<(), Box<dyn std::error::Error>> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_event_loop(&mut terminal, job_id_counter).await;

    // Restore terminal (always, even on error)
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

/// Cached dashboard state polled from GenServers.
#[derive(Default)]
struct DashboardState {
    stats: StatsSnapshot,
    queue: QueueStatus,
    dl_count: usize,
    activity: Vec<String>,
}

/// The main event loop: multiplex keyboard events and a tick timer.
async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    job_id_counter: &'static AtomicU64,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut event_stream = crossterm::event::EventStream::new();
    let mut tick_interval = tokio::time::interval(Duration::from_millis(200));
    let mut state = DashboardState::default();

    // Initial poll
    poll_state(&mut state).await;

    loop {
        // Draw
        terminal.draw(|f| render(f, &state))?;

        tokio::select! {
            _ = tick_interval.tick() => {
                poll_state(&mut state).await;
            }
            event = event_stream.next() => {
                match event {
                    Some(Ok(Event::Key(key))) => {
                        match (key.code, key.modifiers) {
                            (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => break,
                            (KeyCode::Char('c'), KeyModifiers::CONTROL) => break,
                            (KeyCode::Char('+'), _) => enqueue_batch(job_id_counter),
                            (KeyCode::Char('='), _) => add_worker(),
                            (KeyCode::Char('-'), _) => remove_worker(),
                            (_, _) => {}
                        }
                    }
                    Some(Err(_)) => break,
                    None => break,
                    _ => {}
                }
            }
        }
    }

    Ok(())
}

/// Poll all GenServers for current state. Uses short timeouts and
/// falls back to cached values on timeout.
async fn poll_state(state: &mut DashboardState) {
    let timeout = Duration::from_millis(100);

    // Poll stats snapshot
    if let Ok(StatsReply::Snapshot(snapshot)) = ambitious::gen_server::call::<
        StatsCollector,
        StatsReply,
    >("stats", StatsCall::GetSnapshot, timeout)
    .await
    {
        state.stats = snapshot;
    }

    // Poll activity
    if let Ok(StatsReply::Activity(entries)) = ambitious::gen_server::call::<
        StatsCollector,
        StatsReply,
    >("stats", StatsCall::GetActivity(20), timeout)
    .await
    {
        state.activity = entries;
    }

    // Poll queue status
    if let Ok(QueueReply::Status(status)) =
        ambitious::gen_server::call::<JobQueue, QueueReply>("job_queue", QueueCall::Status, timeout)
            .await
    {
        state.queue = status;
    }

    // Poll dead letter count
    if let Ok(DeadLetterReply::Count(count)) = ambitious::gen_server::call::<
        DeadLetterStore,
        DeadLetterReply,
    >("dead_letter", DeadLetterCall::Count, timeout)
    .await
    {
        state.dl_count = count;
    }
}

/// Enqueue a batch of 10 random jobs.
fn enqueue_batch(job_id_counter: &'static AtomicU64) {
    let batch: Vec<Job> = (0..10)
        .map(|_| {
            let id = job_id_counter.fetch_add(1, Ordering::Relaxed);
            Job::new(id, random_job_type(), random_filename())
        })
        .collect();
    ambitious::gen_server::cast::<JobQueue>("job_queue", QueueCast::EnqueueBatch(batch));
}

/// Request the queue to add a new worker.
fn add_worker() {
    ambitious::gen_server::cast::<JobQueue>("job_queue", QueueCast::AddWorker);
}

/// Request the queue to remove an idle worker.
fn remove_worker() {
    ambitious::gen_server::cast::<JobQueue>("job_queue", QueueCast::RemoveWorker);
}

/// Render the full dashboard layout.
fn render(f: &mut Frame, state: &DashboardState) {
    let size = f.area();

    // Main vertical layout
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Length(3), // Queue status bar
            Constraint::Length(5), // Workers panel
            Constraint::Length(3), // Metrics bar
            Constraint::Min(5),    // Activity log
            Constraint::Length(1), // Footer / keybindings
        ])
        .split(size);

    render_header(f, chunks[0]);
    render_queue_status(f, state, chunks[1]);
    render_workers(f, state, chunks[2]);
    render_metrics(f, state, chunks[3]);
    render_activity(f, state, chunks[4]);
    render_footer(f, chunks[5]);
}

/// Render the title header.
fn render_header(f: &mut Frame, area: ratatui::layout::Rect) {
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " Ambitious Job Queue ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("                                    ", Style::default()),
        Span::styled("[q] quit", Style::default().fg(Color::DarkGray)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(header, area);
}

/// Render the queue status summary bar.
fn render_queue_status(f: &mut Frame, state: &DashboardState, area: ratatui::layout::Rect) {
    let line = Line::from(vec![
        Span::styled(" Queue: ", Style::default().fg(Color::White).bold()),
        Span::styled(
            format!("{}", state.queue.pending),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(" pending", Style::default().fg(Color::DarkGray)),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}", state.queue.in_flight),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(" in-flight", Style::default().fg(Color::DarkGray)),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}", state.stats.total_completed),
            Style::default().fg(Color::Green),
        ),
        Span::styled(" completed", Style::default().fg(Color::DarkGray)),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}", state.dl_count),
            Style::default().fg(Color::Red),
        ),
        Span::styled(" DLQ", Style::default().fg(Color::DarkGray)),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let paragraph = Paragraph::new(line).block(block);
    f.render_widget(paragraph, area);
}

/// Render the workers panel.
fn render_workers(f: &mut Frame, state: &DashboardState, area: ratatui::layout::Rect) {
    let total = state.queue.total_workers;
    let idle = state.queue.idle_workers;
    let active = total.saturating_sub(idle);

    let ratio = if total > 0 {
        active as f64 / total as f64
    } else {
        0.0
    };

    let label = format!("{active}/{total} active, {idle} idle");
    let gauge = Gauge::default()
        .block(
            Block::default()
                .title(format!(" Workers ({total}) "))
                .title_style(Style::default().fg(Color::White).bold())
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .gauge_style(
            Style::default()
                .fg(Color::Cyan)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .ratio(ratio)
        .label(label);

    f.render_widget(gauge, area);
}

/// Render the metrics bar.
fn render_metrics(f: &mut Frame, state: &DashboardState, area: ratatui::layout::Rect) {
    let line = Line::from(vec![
        Span::styled(" Throughput: ", Style::default().fg(Color::White).bold()),
        Span::styled(
            format!("{:.1} jobs/sec", state.stats.throughput),
            Style::default().fg(Color::Green),
        ),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::styled("Avg: ", Style::default().fg(Color::White).bold()),
        Span::styled(
            format!("{:.0}ms", state.stats.avg_duration_ms),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::styled("Failed: ", Style::default().fg(Color::White).bold()),
        Span::styled(
            format!("{}", state.stats.total_failed),
            Style::default().fg(Color::Red),
        ),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::styled("Retried: ", Style::default().fg(Color::White).bold()),
        Span::styled(
            format!("{}", state.stats.total_retried),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::styled("Enqueued: ", Style::default().fg(Color::White).bold()),
        Span::styled(
            format!("{}", state.stats.total_enqueued),
            Style::default().fg(Color::Cyan),
        ),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let paragraph = Paragraph::new(line).block(block);
    f.render_widget(paragraph, area);
}

/// Render the recent activity log.
fn render_activity(f: &mut Frame, state: &DashboardState, area: ratatui::layout::Rect) {
    let items: Vec<ListItem> = state
        .activity
        .iter()
        .map(|entry| {
            let style = if entry.contains("completed") {
                Style::default().fg(Color::Green)
            } else if entry.contains("failed") {
                Style::default().fg(Color::Red)
            } else if entry.contains("retry") {
                Style::default().fg(Color::Yellow)
            } else if entry.contains("dead letter") {
                Style::default().fg(Color::Magenta)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(format!("  {entry}")).style(style)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .title(" Recent Activity ")
            .title_style(Style::default().fg(Color::White).bold())
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(list, area);
}

/// Render the footer with keybindings.
fn render_footer(f: &mut Frame, area: ratatui::layout::Rect) {
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" [+] ", Style::default().fg(Color::Cyan).bold()),
        Span::styled("Add 10 jobs", Style::default().fg(Color::DarkGray)),
        Span::styled("  [=] ", Style::default().fg(Color::Cyan).bold()),
        Span::styled("Add worker", Style::default().fg(Color::DarkGray)),
        Span::styled("  [-] ", Style::default().fg(Color::Cyan).bold()),
        Span::styled("Remove worker", Style::default().fg(Color::DarkGray)),
        Span::styled("  [q] ", Style::default().fg(Color::Cyan).bold()),
        Span::styled("Quit", Style::default().fg(Color::DarkGray)),
    ]));
    f.render_widget(footer, area);
}
