use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};

pub const ACCENT: Color = Color::Rgb(110, 220, 210);
pub const BACKGROUND: Color = Color::Rgb(17, 23, 36);
pub const PANEL: Color = Color::Rgb(24, 33, 49);
pub const MUTED: Color = Color::Rgb(145, 160, 180);
pub const SELECTED: Color = Color::Rgb(38, 67, 82);

pub fn panel(title: impl Into<Line<'static>>) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(66, 88, 110)))
        .title_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
        .style(Style::default().bg(PANEL).fg(Color::White))
        .title(title)
}

/// Shrink `area` to a centered box no larger than `max`, keeping a margin.
pub fn centered(area: Rect, max: (u16, u16)) -> Rect {
    let width = area.width.saturating_sub(4).min(max.0);
    let height = area.height.saturating_sub(2).min(max.1);
    let vertical = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .split(area);
    let horizontal = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(width),
        Constraint::Fill(1),
    ])
    .split(vertical[1]);
    horizontal[1]
}

/// Render a modal help panel; `?` toggles it on either screen.
pub fn help(frame: &mut ratatui::Frame<'_>, title: &str, lines: Vec<Line<'_>>) {
    let area = centered(frame.area(), (72, 30));
    frame.render_widget(Clear, area);
    let rows = Layout::vertical([Constraint::Fill(1), Constraint::Length(3)]).split(area);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .style(Style::default().bg(PANEL).fg(Color::White))
                .border_style(Style::default().fg(Color::Cyan))
                .title(format!(" {title} ")),
        ),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " Press ? or Esc to close help ",
            Style::default().fg(Color::Gray),
        )))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .style(Style::default().bg(PANEL).fg(Color::White))
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        rows[1],
    );
}

pub fn section(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_owned(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))
}

pub fn keys<'a>(keys: &'a str, description: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("  {keys:<22}"), Style::default().fg(Color::Yellow)),
        Span::raw(description),
    ])
}
