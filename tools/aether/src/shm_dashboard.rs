//! TUI Dashboard for shared memory monitoring (htop-style)
//!
//! Extracted from shm.rs to keep each module focused on a single concern.

use anyhow::{Context, Result};
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::shm::{ShmKey, ShmRuntimeView, get_value, open_reader, parse_key};

/// Point data for display in TUI
struct PointRow {
    key: String,
    kind: &'static str,
    value: f64,
}

/// Dashboard application state
struct DashboardState {
    points: Vec<PointRow>,
    table_state: TableState,
    scroll_offset: usize,
    last_scan: Instant,
    last_instance_count: usize,
    last_channel_count: usize,
}

impl DashboardState {
    fn new() -> Self {
        Self {
            points: Vec::new(),
            table_state: TableState::default(),
            scroll_offset: 0,
            last_scan: Instant::now(),
            last_instance_count: 0,
            last_channel_count: 0,
        }
    }

    fn scroll_up(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset -= 1;
        }
    }

    fn scroll_down(&mut self, max: usize) {
        if self.scroll_offset < max.saturating_sub(1) {
            self.scroll_offset += 1;
        }
    }
}

/// Run the TUI dashboard
pub async fn run_dashboard(data_directory: &Path) -> Result<()> {
    let reader = open_reader(data_directory).await?;
    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    stdout
        .execute(EnterAlternateScreen)
        .context("Failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Failed to create terminal")?;

    let mut state = DashboardState::new();
    let tick_rate = Duration::from_millis(250);

    let result = run_dashboard_loop(&mut terminal, &reader, &mut state, tick_rate);

    disable_raw_mode().context("Failed to disable raw mode")?;
    terminal
        .backend_mut()
        .execute(LeaveAlternateScreen)
        .context("Failed to leave alternate screen")?;

    result
}

fn run_dashboard_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    reader: &ShmRuntimeView,
    state: &mut DashboardState,
    tick_rate: Duration,
) -> Result<()> {
    let mut last_tick = Instant::now();

    loop {
        refresh_point_data(reader, state);
        terminal.draw(|f| draw_dashboard(f, reader, state))?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout).context("Failed to poll events")?
            && let Event::Key(key) = event::read().context("Failed to read event")?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Up | KeyCode::Char('k') => state.scroll_up(),
                KeyCode::Down | KeyCode::Char('j') => state.scroll_down(state.points.len()),
                KeyCode::Char('r') => {
                    state.last_instance_count = 0;
                    state.last_channel_count = 0;
                },
                _ => {},
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }
    }
}

fn refresh_point_data(reader: &ShmRuntimeView, state: &mut DashboardState) {
    let instance_count = reader.instance_ids().len();
    let channel_count = reader.channel_ids().len();

    let should_rescan = instance_count != state.last_instance_count
        || channel_count != state.last_channel_count
        || state.last_scan.elapsed() > Duration::from_secs(5);

    if should_rescan {
        state.points = collect_all_points(reader);
        state.last_instance_count = instance_count;
        state.last_channel_count = channel_count;
        state.last_scan = Instant::now();
    } else {
        update_point_values(reader, &mut state.points);
    }
}

fn collect_all_points(reader: &ShmRuntimeView) -> Vec<PointRow> {
    let mut rows = Vec::new();

    for key in reader.named_keys() {
        let kind = match &key {
            ShmKey::Instance { point_type: 0, .. } => "M",
            ShmKey::Instance { .. } => "A",
            ShmKey::Channel { point_type, .. } => point_type.as_str(),
        };
        if let Ok(Some(value)) = get_value(reader, &key) {
            rows.push(PointRow {
                key: key.to_string(),
                kind,
                value,
            });
        }
    }

    rows
}

fn update_point_values(reader: &ShmRuntimeView, points: &mut [PointRow]) {
    for point in points.iter_mut() {
        if let Ok(key) = parse_key(&point.key)
            && let Ok(Some(value)) = get_value(reader, &key)
        {
            point.value = value;
        }
    }
}

fn draw_dashboard(f: &mut ratatui::Frame, reader: &ShmRuntimeView, state: &DashboardState) {
    let alive = reader.is_writer_alive(Duration::from_secs(5));
    let heartbeat = reader.writer_heartbeat();
    let heartbeat_age = aether_dataplane::core::config::timestamp_ms().saturating_sub(heartbeat);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(5)])
        .split(f.area());

    let writer_status = if alive {
        format!("● alive ({}ms)", heartbeat_age)
    } else {
        format!("○ dead ({}ms)", heartbeat_age)
    };

    let status_text = format!(
        " Instances: {}  Channels: {}  Points: {}  Writer: {}  │  [q]uit [↑↓]scroll [r]efresh",
        reader.instance_ids().len(),
        reader.channel_ids().len(),
        state.points.len(),
        writer_status
    );

    let status_style = if alive {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::Red)
    };

    let status = Paragraph::new(status_text).style(status_style).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Aether Shared Memory Monitor "),
    );
    f.render_widget(status, chunks[0]);

    let header_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    let header = Row::new(["Key", "Type", "Value"])
        .style(header_style)
        .height(1);

    let visible_rows: Vec<Row> = state
        .points
        .iter()
        .skip(state.scroll_offset)
        .map(|p| {
            let value_str = format!("{:.6}", p.value);
            Row::new(vec![
                Cell::from(p.key.as_str()),
                Cell::from(p.kind),
                Cell::from(value_str),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(20),
        Constraint::Length(6),
        Constraint::Min(15),
    ];

    let table = Table::new(visible_rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(format!(
            " Points ({}/{}) ",
            state.scroll_offset + 1,
            state.points.len().max(1)
        )))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    f.render_stateful_widget(table, chunks[1], &mut state.table_state.clone());
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn test_dashboard_state_new() {
        let state = DashboardState::new();
        assert!(state.points.is_empty());
        assert_eq!(state.scroll_offset, 0);
        assert_eq!(state.last_instance_count, 0);
        assert_eq!(state.last_channel_count, 0);
    }

    #[test]
    fn test_dashboard_state_scroll_up() {
        let mut state = DashboardState::new();
        state.scroll_offset = 5;

        state.scroll_up();
        assert_eq!(state.scroll_offset, 4);

        state.scroll_offset = 0;
        state.scroll_up();
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_dashboard_state_scroll_down() {
        let mut state = DashboardState::new();

        state.scroll_down(10);
        assert_eq!(state.scroll_offset, 1);

        state.scroll_offset = 8;
        state.scroll_down(10);
        assert_eq!(state.scroll_offset, 9);

        state.scroll_offset = 9;
        state.scroll_down(10);
        assert_eq!(state.scroll_offset, 9);
    }

    #[test]
    fn test_dashboard_state_scroll_down_empty() {
        let mut state = DashboardState::new();
        state.scroll_down(0);
        assert_eq!(state.scroll_offset, 0);
    }
}
