//! Interactive TUI log viewer for AetherEMS service logs
//!
//! `aether logs ui <service>` — scrollable, searchable, follow-mode log viewer.
//! Keys: ↑↓/jk scroll | g/G top/bottom | f follow | / search | n next | q quit

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
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// TUI viewer mode
#[derive(PartialEq)]
enum Mode {
    Normal,
    Search,
    Follow,
}

/// Application state for the log TUI
struct LogViewerState {
    lines: Vec<String>,
    filtered_indices: Vec<usize>,
    scroll_offset: usize,
    mode: Mode,
    search_input: String,
    active_search: String,
    search_matches: Vec<usize>,
    search_cursor: usize,
    file_path: PathBuf,
    file_position: u64,
    status_msg: String,
}

impl LogViewerState {
    fn new(path: PathBuf, lines: Vec<String>) -> Self {
        let len = lines.len();
        let filtered_indices: Vec<usize> = (0..len).collect();
        Self {
            lines,
            filtered_indices,
            scroll_offset: 0,
            mode: Mode::Normal,
            search_input: String::new(),
            active_search: String::new(),
            search_matches: Vec::new(),
            search_cursor: 0,
            file_path: path,
            file_position: 0,
            status_msg: String::new(),
        }
    }

    fn visible_count(&self) -> usize {
        self.filtered_indices.len()
    }

    fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    fn scroll_down(&mut self, n: usize, viewport_height: usize) {
        let max = self.visible_count().saturating_sub(viewport_height);
        self.scroll_offset = (self.scroll_offset + n).min(max);
    }

    fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
    }

    fn scroll_to_bottom(&mut self, viewport_height: usize) {
        let max = self.visible_count().saturating_sub(viewport_height);
        self.scroll_offset = max;
    }

    fn apply_search(&mut self) {
        self.active_search = self.search_input.clone();
        if self.active_search.is_empty() {
            self.search_matches.clear();
            self.search_cursor = 0;
            self.status_msg = String::new();
            return;
        }
        let pattern = self.active_search.to_lowercase();
        self.search_matches = self
            .filtered_indices
            .iter()
            .copied()
            .filter(|&i| self.lines[i].to_lowercase().contains(&pattern))
            .collect();
        self.search_cursor = 0;
        let count = self.search_matches.len();
        self.status_msg = if count == 0 {
            format!("Pattern not found: {}", self.active_search)
        } else {
            format!("{} matches", count)
        };
    }

    fn jump_to_match(&mut self, viewport_height: usize) {
        if self.search_matches.is_empty() {
            return;
        }
        let line_idx = self.search_matches[self.search_cursor];
        // Find position in filtered_indices
        if let Some(pos) = self.filtered_indices.iter().position(|&i| i == line_idx) {
            // Center the match in viewport
            self.scroll_offset = pos.saturating_sub(viewport_height / 2);
            let max = self.visible_count().saturating_sub(viewport_height);
            self.scroll_offset = self.scroll_offset.min(max);
        }
        self.status_msg = format!(
            "Match {}/{}",
            self.search_cursor + 1,
            self.search_matches.len()
        );
    }

    fn next_match(&mut self, viewport_height: usize) {
        if self.search_matches.is_empty() {
            return;
        }
        self.search_cursor = (self.search_cursor + 1) % self.search_matches.len();
        self.jump_to_match(viewport_height);
    }

    fn poll_new_lines(&mut self, viewport_height: usize) -> Result<()> {
        let mut file = std::fs::File::open(&self.file_path)?;
        file.seek(SeekFrom::Start(self.file_position))?;
        let mut reader = BufReader::new(file);
        let mut buf = String::new();
        let mut added = false;
        while reader.read_line(&mut buf)? > 0 {
            let line = buf
                .trim_end_matches('\n')
                .trim_end_matches('\r')
                .to_string();
            let idx = self.lines.len();
            self.lines.push(line);
            self.filtered_indices.push(idx);
            added = true;
            buf.clear();
        }
        self.file_position = reader.into_inner().stream_position()?;
        if added && self.mode == Mode::Follow {
            self.scroll_to_bottom(viewport_height);
        }
        Ok(())
    }
}

/// Entry point: run the interactive log viewer TUI.
pub fn run_log_viewer(path: &Path) -> Result<()> {
    // Load all lines from file
    let file =
        std::fs::File::open(path).with_context(|| format!("Cannot open {}", path.display()))?;
    let file_len = file.metadata()?.len();
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();

    let mut state = LogViewerState::new(path.to_path_buf(), lines);
    state.file_position = file_len;
    // Start at bottom
    state.scroll_to_bottom(24); // approximate, will correct on first draw

    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    stdout
        .execute(EnterAlternateScreen)
        .context("Failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Failed to create terminal")?;

    let result = run_viewer_loop(&mut terminal, &mut state);

    disable_raw_mode().context("Failed to disable raw mode")?;
    terminal
        .backend_mut()
        .execute(LeaveAlternateScreen)
        .context("Failed to leave alternate screen")?;

    result
}

fn run_viewer_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut LogViewerState,
) -> Result<()> {
    let tick_rate = Duration::from_millis(200);
    let mut last_tick = Instant::now();
    let mut viewport_height: usize = 24;

    loop {
        terminal.draw(|f| {
            viewport_height = draw_viewer(f, state);
        })?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout).context("Failed to poll events")?
            && let Event::Key(key) = event::read().context("Failed to read event")?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match state.mode {
                Mode::Search => match key.code {
                    KeyCode::Enter => {
                        state.apply_search();
                        state.jump_to_match(viewport_height);
                        state.mode = Mode::Normal;
                    },
                    KeyCode::Esc => {
                        state.search_input.clear();
                        state.mode = Mode::Normal;
                        state.status_msg = String::new();
                    },
                    KeyCode::Backspace => {
                        state.search_input.pop();
                    },
                    KeyCode::Char(c) => {
                        state.search_input.push(c);
                    },
                    _ => {},
                },
                Mode::Normal | Mode::Follow => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Up | KeyCode::Char('k') => {
                        state.mode = Mode::Normal;
                        state.scroll_up(1);
                    },
                    KeyCode::Down | KeyCode::Char('j') => {
                        state.mode = Mode::Normal;
                        state.scroll_down(1, viewport_height);
                    },
                    KeyCode::PageUp => {
                        state.mode = Mode::Normal;
                        state.scroll_up(viewport_height);
                    },
                    KeyCode::PageDown => {
                        state.mode = Mode::Normal;
                        state.scroll_down(viewport_height, viewport_height);
                    },
                    KeyCode::Char('g') => {
                        state.mode = Mode::Normal;
                        state.scroll_to_top();
                    },
                    KeyCode::Char('G') => {
                        state.scroll_to_bottom(viewport_height);
                    },
                    KeyCode::Char('f') => {
                        if state.mode == Mode::Follow {
                            state.mode = Mode::Normal;
                            state.status_msg = "Follow OFF".to_string();
                        } else {
                            state.mode = Mode::Follow;
                            state.scroll_to_bottom(viewport_height);
                            state.status_msg = "Follow ON".to_string();
                        }
                    },
                    KeyCode::Char('/') => {
                        state.mode = Mode::Search;
                        state.search_input.clear();
                    },
                    KeyCode::Char('n') => {
                        state.next_match(viewport_height);
                    },
                    _ => {},
                },
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
            // Poll for new lines in follow mode (or even normal — no harm)
            let _ = state.poll_new_lines(viewport_height);
        }
    }
}

/// Draw the log viewer UI. Returns the viewport height (number of visible log lines).
fn draw_viewer(f: &mut ratatui::Frame, state: &LogViewerState) -> usize {
    let area = f.area();

    // Layout: [status bar 1] [log content] [help bar 1]
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    // ── Status bar ──
    let mode_indicator = match state.mode {
        Mode::Normal => "NORMAL",
        Mode::Search => "SEARCH",
        Mode::Follow => "FOLLOW",
    };
    let mode_color = match state.mode {
        Mode::Normal => Color::Cyan,
        Mode::Search => Color::Yellow,
        Mode::Follow => Color::Green,
    };

    let file_name = state
        .file_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();

    let status_line = Line::from(vec![
        Span::styled(
            format!(" {} ", mode_indicator),
            Style::default()
                .fg(Color::Black)
                .bg(mode_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            file_name.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "  {} lines  offset {}",
            state.visible_count(),
            state.scroll_offset
        )),
        Span::raw("  "),
        Span::styled(&state.status_msg, Style::default().fg(Color::Yellow)),
    ]);
    f.render_widget(
        Paragraph::new(status_line).style(Style::default().bg(Color::DarkGray)),
        chunks[0],
    );

    // ── Log content ──
    let content_height = chunks[1].height.saturating_sub(2) as usize; // borders take 2
    let inner_height = content_height;

    let search_lower = if state.active_search.is_empty() {
        None
    } else {
        Some(state.active_search.to_lowercase())
    };

    let visible_lines: Vec<Line> = state
        .filtered_indices
        .iter()
        .skip(state.scroll_offset)
        .take(inner_height)
        .map(|&idx| {
            let raw = &state.lines[idx];
            colorize_log_line(raw, search_lower.as_deref())
        })
        .collect();

    let log_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(visible_lines).block(log_block);
    f.render_widget(paragraph, chunks[1]);

    // ── Scrollbar ──
    let total = state.visible_count();
    if total > inner_height {
        let mut scrollbar_state =
            ScrollbarState::new(total.saturating_sub(inner_height)).position(state.scroll_offset);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            chunks[1],
            &mut scrollbar_state,
        );
    }

    // ── Help / search input bar ──
    let help_line = match state.mode {
        Mode::Search => Line::from(vec![
            Span::styled(" /", Style::default().fg(Color::Yellow)),
            Span::raw(&state.search_input),
            Span::styled("█", Style::default().fg(Color::Yellow)),
            Span::raw("  "),
            Span::styled(
                "[Enter] search  [Esc] cancel",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        _ => Line::from(vec![
            Span::styled(" ↑↓", Style::default().fg(Color::Cyan)),
            Span::styled(" scroll ", Style::default().fg(Color::DarkGray)),
            Span::styled("g/G", Style::default().fg(Color::Cyan)),
            Span::styled(" top/bottom ", Style::default().fg(Color::DarkGray)),
            Span::styled("f", Style::default().fg(Color::Cyan)),
            Span::styled(" follow ", Style::default().fg(Color::DarkGray)),
            Span::styled("/", Style::default().fg(Color::Cyan)),
            Span::styled(" search ", Style::default().fg(Color::DarkGray)),
            Span::styled("n", Style::default().fg(Color::Cyan)),
            Span::styled(" next ", Style::default().fg(Color::DarkGray)),
            Span::styled("q", Style::default().fg(Color::Cyan)),
            Span::styled(" quit", Style::default().fg(Color::DarkGray)),
        ]),
    };
    f.render_widget(Paragraph::new(help_line), chunks[2]);

    inner_height
}

/// Colorize a log line based on level and highlight search matches.
fn colorize_log_line<'a>(line: &'a str, search: Option<&str>) -> Line<'a> {
    let base_style = if line.contains("[ERROR]") || line.contains(" ERROR ") {
        Style::default().fg(Color::Red)
    } else if line.contains("[WARN]") || line.contains(" WARN ") {
        Style::default().fg(Color::Yellow)
    } else if line.contains("[DEBUG]")
        || line.contains(" DEBUG ")
        || line.contains("[TRACE]")
        || line.contains(" TRACE ")
    {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    };

    // If no search or search not found in this line, return single span
    let search = match search {
        Some(s) if !s.is_empty() => s,
        _ => return Line::from(Span::styled(line, base_style)),
    };

    let lower = line.to_lowercase();
    if !lower.contains(search) {
        return Line::from(Span::styled(line, base_style));
    }

    // Highlight search matches
    let highlight = base_style
        .bg(Color::Yellow)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    let mut cursor = 0;

    for (start, _) in lower.match_indices(search) {
        if start > cursor {
            spans.push(Span::styled(&line[cursor..start], base_style));
        }
        spans.push(Span::styled(&line[start..start + search.len()], highlight));
        cursor = start + search.len();
    }
    if cursor < line.len() {
        spans.push(Span::styled(&line[cursor..], base_style));
    }

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scroll_up_at_zero() {
        let mut state = LogViewerState::new(PathBuf::from("test.log"), vec![]);
        state.scroll_up(1);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_scroll_down_clamps() {
        let lines = vec!["a".into(), "b".into(), "c".into()];
        let mut state = LogViewerState::new(PathBuf::from("test.log"), lines);
        // viewport = 10, only 3 lines → max offset = 0
        state.scroll_down(5, 10);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_scroll_down_with_content() {
        let lines: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        let mut state = LogViewerState::new(PathBuf::from("test.log"), lines);
        state.scroll_down(10, 20);
        assert_eq!(state.scroll_offset, 10);
        // Max = 50 - 20 = 30
        state.scroll_down(100, 20);
        assert_eq!(state.scroll_offset, 30);
    }

    #[test]
    fn test_scroll_to_top_and_bottom() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        let mut state = LogViewerState::new(PathBuf::from("test.log"), lines);
        state.scroll_to_bottom(20);
        assert_eq!(state.scroll_offset, 80);
        state.scroll_to_top();
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_search_apply() {
        let lines = vec![
            "INFO all good".into(),
            "ERROR bad stuff".into(),
            "WARN maybe".into(),
            "ERROR another".into(),
        ];
        let mut state = LogViewerState::new(PathBuf::from("test.log"), lines);
        state.search_input = "error".into();
        state.apply_search();
        assert_eq!(state.search_matches.len(), 2);
        assert_eq!(state.search_matches[0], 1);
        assert_eq!(state.search_matches[1], 3);
        assert!(state.status_msg.contains("2 matches"));
    }

    #[test]
    fn test_search_not_found() {
        let lines = vec!["INFO ok".into()];
        let mut state = LogViewerState::new(PathBuf::from("test.log"), lines);
        state.search_input = "xyz".into();
        state.apply_search();
        assert!(state.search_matches.is_empty());
        assert!(state.status_msg.contains("not found"));
    }

    #[test]
    fn test_next_match_wraps() {
        let lines = vec!["a".into(), "ERROR x".into(), "b".into(), "ERROR y".into()];
        let mut state = LogViewerState::new(PathBuf::from("test.log"), lines);
        state.search_input = "error".into();
        state.apply_search();
        assert_eq!(state.search_cursor, 0);
        state.next_match(20);
        assert_eq!(state.search_cursor, 1);
        state.next_match(20);
        assert_eq!(state.search_cursor, 0); // wraps
    }

    #[test]
    fn test_colorize_plain_line() {
        let line = "INFO normal line";
        let result = colorize_log_line(line, None);
        assert_eq!(result.spans.len(), 1);
    }

    #[test]
    fn test_colorize_with_search_highlight() {
        let line = "ERROR found something ERROR again";
        let result = colorize_log_line(line, Some("error"));
        // Should have multiple spans due to search highlighting
        assert!(result.spans.len() > 1);
    }
}
