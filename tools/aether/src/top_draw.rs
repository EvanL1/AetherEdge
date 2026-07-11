//! TUI drawing functions for `aether top`
//!
//! Separated from `top.rs` to keep each module under 800 lines.

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs};

use crate::top::{Depth, TopState, View, format_time, is_stale};

pub fn draw_ui(f: &mut ratatui::Frame, state: &TopState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header + tabs
            Constraint::Min(5),    // main content
            Constraint::Length(1), // help
        ])
        .split(f.area());

    draw_header(f, chunks[0], state);

    match (state.view, state.depth) {
        (View::Channels, Depth::List) => draw_channel_list(f, chunks[1], state),
        (View::Channels, Depth::Detail) => draw_channel_detail(f, chunks[1], state),
        (View::Instances, Depth::List) => draw_instance_list(f, chunks[1], state),
        (View::Instances, Depth::Detail) => draw_instance_detail(f, chunks[1], state),
        (View::Rules, _) => draw_rule_list(f, chunks[1], state),
    }

    draw_help(f, chunks[2], state);
}

fn draw_header(f: &mut ratatui::Frame, area: ratatui::layout::Rect, state: &TopState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(2)])
        .split(area);

    let titles: Vec<ratatui::text::Line> = View::ALL
        .iter()
        .map(|v| ratatui::text::Line::from(format!(" {} ", v.label())))
        .collect();

    let tabs = Tabs::new(titles)
        .select(state.view.index())
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .divider("│");
    f.render_widget(tabs, chunks[0]);

    let status = if let Some(err) = &state.error_msg {
        format!(" ⚠ {err}")
    } else {
        let online = state.channels.iter().filter(|c| c.connected).count();
        let shm = if state.shm_ok { "✓" } else { "✗" };
        format!(
            " {} — {}ch ({}on) {}inst {}rules shm{}",
            state.host_display,
            state.channels.len(),
            online,
            state.instances.len(),
            state.rules.len(),
            shm,
        )
    };
    let style = if state.error_msg.is_some() {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    f.render_widget(Paragraph::new(status).style(style), chunks[1]);
}

fn draw_channel_list(f: &mut ratatui::Frame, area: ratatui::layout::Rect, state: &TopState) {
    let header = Row::new([
        "ID", "St", "Name", "Protocol", "Address", "Pts", "Reads", "Errs",
    ])
    .style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = state
        .channels
        .iter()
        .map(|ch| {
            let st = if ch.connected { "✓" } else { "✗" };
            let st_style = if ch.connected {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Red)
            };
            let err = if ch.error_count > 0 {
                ch.error_count.to_string()
            } else {
                String::new()
            };
            Row::new(vec![
                Cell::from(format!("Ch{}", ch.id)),
                Cell::from(st).style(st_style),
                Cell::from(ch.name.as_str()),
                Cell::from(ch.protocol.as_str()),
                Cell::from(ch.address.as_str()),
                Cell::from(ch.point_count.to_string()),
                Cell::from(ch.read_count.to_string()),
                Cell::from(err).style(Style::default().fg(Color::Yellow)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(5),
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(12),
        Constraint::Length(20),
        Constraint::Length(5),
        Constraint::Length(7),
        Constraint::Length(6),
    ];

    let title = format!(
        " Channels ({}) — Enter to view points ",
        state.channels.len()
    );
    let table = Table::new(rows, widths)
        .header(header)
        .block(focused_block(&title, true))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("► ");

    let mut tbl = state.ch_table.clone();
    f.render_stateful_widget(table, area, &mut tbl);
}

fn draw_channel_detail(f: &mut ratatui::Frame, area: ratatui::layout::Rect, state: &TopState) {
    let ch_name = state
        .ch_table
        .selected()
        .and_then(|i| state.channels.get(i))
        .map(|ch| format!("Ch{}: {}", ch.id, ch.name))
        .unwrap_or_default();

    let visible = state.visible_points();
    let zero_label = if state.hide_zero { "hide" } else { "show" };
    let title = format!(
        " {ch_name} — {} pts (zero={zero_label}) — Esc to back ",
        visible.len()
    );

    let header = Row::new(["ID", "Name", "Value", "Unit", "Updated"]).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = visible
        .iter()
        .map(|p| {
            let stale = is_stale(p.ts_ms, 10);
            let val_style = if stale {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            };
            let age = format_time(p.ts_ms);
            let age_style = if stale {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Row::new(vec![
                Cell::from(format!("pt{}", p.point_id)),
                Cell::from(p.name.as_str()),
                Cell::from(fmt_val(p.value)).style(val_style),
                Cell::from(p.unit.as_str()),
                Cell::from(age).style(age_style),
            ])
        })
        .collect();

    // ID(6) + Name(flex) + Value(10) + Unit(5) + Updated(15) = 36 fixed + Name
    // Name absorbs remaining space; ratatui auto-clips if too narrow
    let widths = [
        Constraint::Length(6),
        Constraint::Fill(1),
        Constraint::Length(10),
        Constraint::Length(5),
        Constraint::Length(15),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(focused_block(&title, true))
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("► ");

    let mut tbl = state.pt_table.clone();
    f.render_stateful_widget(table, area, &mut tbl);
}

fn draw_instance_list(f: &mut ratatui::Frame, area: ratatui::layout::Rect, state: &TopState) {
    let header = Row::new(["ID", "Name", "Product"]).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = state
        .instances
        .iter()
        .map(|inst| {
            Row::new(vec![
                Cell::from(inst.id.to_string()),
                Cell::from(inst.name.as_str()),
                Cell::from(inst.product.as_str()),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(6),
        Constraint::Fill(1),
        Constraint::Length(15),
    ];

    let title = format!(
        " Instances ({}) — Enter to view data ",
        state.instances.len()
    );
    let table = Table::new(rows, widths)
        .header(header)
        .block(focused_block(&title, true))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("► ");

    let mut tbl = state.inst_table.clone();
    f.render_stateful_widget(table, area, &mut tbl);
}

fn draw_instance_detail(f: &mut ratatui::Frame, area: ratatui::layout::Rect, state: &TopState) {
    let inst_name = state
        .inst_table
        .selected()
        .and_then(|i| state.instances.get(i))
        .map(|inst| format!("{} ({})", inst.name, inst.product))
        .unwrap_or_default();

    let title = format!(
        " {} — {} points — Esc to back ",
        inst_name,
        state.inst_points.len()
    );

    let header = Row::new(["ID", "Type", "Name", "Value", "Unit", "Routing", "Updated"]).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = state
        .inst_points
        .iter()
        .map(|p| {
            let stale = is_stale(p.ts_ms, 10);
            let val_style = if stale {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            };
            let route_style = if p.routing.is_empty() || stale {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::Cyan)
            };
            let age = format_time(p.ts_ms);
            let age_style = if stale {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Row::new(vec![
                Cell::from(p.point_id.to_string()),
                Cell::from(p.kind),
                Cell::from(p.name.as_str()),
                Cell::from(fmt_val(p.value)).style(val_style),
                Cell::from(p.unit.as_str()),
                Cell::from(if p.routing.is_empty() {
                    "-"
                } else {
                    p.routing.as_str()
                })
                .style(route_style),
                Cell::from(age).style(age_style),
            ])
        })
        .collect();

    // ID(4) + Type(2) + Name(flex) + Value(10) + Unit(5) + Routing(13) + Updated(15)
    let widths = [
        Constraint::Length(4),
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(10),
        Constraint::Length(5),
        Constraint::Length(13),
        Constraint::Length(15),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(focused_block(&title, true))
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("► ");

    let mut tbl = state.inst_pt_table.clone();
    f.render_stateful_widget(table, area, &mut tbl);
}

fn draw_rule_list(f: &mut ratatui::Frame, area: ratatui::layout::Rect, state: &TopState) {
    let header = Row::new(["ID", "Status", "Name", "Description"]).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = state
        .rules
        .iter()
        .map(|r| {
            let st = if r.enabled { "✓" } else { "✗" };
            let st_style = if r.enabled {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Red)
            };
            Row::new(vec![
                Cell::from(r.id.to_string()),
                Cell::from(st).style(st_style),
                Cell::from(r.name.as_str()),
                Cell::from(r.description.as_str()),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(5),
        Constraint::Length(6),
        Constraint::Min(25),
        Constraint::Min(30),
    ];

    let title = format!(" Rules ({}) ", state.rules.len());
    let table = Table::new(rows, widths)
        .header(header)
        .block(focused_block(&title, true))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("► ");

    let mut tbl = state.rule_table.clone();
    f.render_stateful_widget(table, area, &mut tbl);
}

pub fn draw_help(f: &mut ratatui::Frame, area: ratatui::layout::Rect, state: &TopState) {
    let help = match (state.view, state.depth) {
        (_, Depth::Detail) => " [Esc/←]Back [↑↓]Scroll [z]Zero [r]Refresh [q]Quit ",
        _ => " [←→/Tab]View [↑↓]Select [Enter]Detail [z]Zero [r]Refresh [q]Quit [1/2/3]Jump ",
    };
    f.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn focused_block<'a>(title: &'a str, focused: bool) -> Block<'a> {
    let style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(title)
}

pub fn fmt_val(v: f64) -> String {
    if v.is_nan() {
        // NaN is the in-memory marker for "no data fetched" (SHM/API miss /
        // unwritten SHM slot). Render distinctly so the operator can tell
        // "missing" from a real device reading of zero — these used to
        // collapse into the same "0" cell and silently hide.
        "—".to_string()
    } else if v == 0.0 {
        "0".to_string()
    } else if v.fract() == 0.0 && v.abs() < 1_000_000.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.2}")
    }
}
