use crate::{downloads::Downloads, tui};
use anyhow::{bail, Result};
use contextd_core::{models, Service};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, Gauge, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};
use std::{
    io::{self, IsTerminal},
    sync::Arc,
    time::Duration,
};

struct RestoreTerminal;
impl Drop for RestoreTerminal {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

pub fn run(
    service: Arc<Service>,
    runtime: tokio::runtime::Handle,
    downloads: Arc<Downloads>,
) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("Configuration needs an interactive terminal. Use contextd config --json to display settings.");
    }
    let _restore = RestoreTerminal;
    let mut terminal = ratatui::try_init()?;
    let mut tab = 0;
    let mut selection = ListState::default().with_selected(Some(0));
    let mut help = false;
    let mut message = "Select a model with Enter; d deletes a cached copy".to_string();
    loop {
        let settings = service.settings();
        let catalog: Vec<_> = models::catalog(&service.paths)
            .into_iter()
            .filter(|model| model.kind == if tab == 0 { "speech" } else { "filter" })
            .collect();
        let selected = selection.selected().unwrap_or(0).min(catalog.len() - 1);
        selection.select(Some(selected));
        let active = downloads.snapshot();
        terminal.draw(|frame| {
            let rows = Layout::vertical([
                Constraint::Length(1), Constraint::Length(3), Constraint::Min(5),
                Constraint::Length(3), Constraint::Length(6), Constraint::Length(2),
            ]).split(frame.area());
            frame.render_widget(Paragraph::new(format!("contextd — Configuration · {}", service.paths.settings_path().display())), rows[0]);
            frame.render_widget(Tabs::new(["Speech to text", "Cleanup / processing"])
                .select(tab).highlight_style(Style::default().fg(Color::Cyan))
                .block(Block::bordered().title("Tab / h / l: switch")), rows[1]);
            let columns = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).split(rows[2]);
            let items: Vec<_> = catalog.iter().map(|model| {
                let active = model.id == settings.speech_model || model.kind == "filter" && model.id == models::canonical_cleanup_id(&settings.filter_model);
                let state = if active {
                    "selected"
                } else if downloads.queued(&model.id) {
                    "queued"
                } else if model.installed {
                    "cached"
                } else {
                    "download"
                };
                ListItem::new(format!("{} {} [{state}]", if active { "●" } else { " " }, model.name))
            }).collect();
            frame.render_stateful_widget(List::new(items).highlight_symbol("› ")
                .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
                .block(Block::bordered().title("Models · j / k")), columns[0], &mut selection);
            let model = &catalog[selected];
            frame.render_widget(Paragraph::new(format!("{}\n\n{}\n\nApproximate download: {} MB\nCache: {}\n\nEnter: select and download in the background\nd: delete the cached copy", model.name, model.description, model.size_mb,
                service.paths.models_dir().display()))
                .wrap(Wrap { trim: false }).block(Block::bordered().title(format!(" Model details · {} ", message))), columns[1]);
            frame.render_widget(Paragraph::new(format!("Cleanup: {} (c: toggle) · Retain audio: {} (a: toggle)",
                if settings.filter_enabled { "enabled" } else { "disabled" }, settings.retain_audio))
                .block(Block::bordered().title("Settings")), rows[3]);
            let progress = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).split(rows[4]);
            if let Some((id, first)) = active.first() {
                let ratio = if first.total == 0 { 0.0 } else { (first.completed as f64 / first.total as f64).min(1.0) };
                let queued = downloads.queued(id);
                frame.render_widget(Gauge::default().ratio(ratio).gauge_style(Style::default().fg(Color::Cyan))
                    .label(if queued { format!("{id}: waiting for a free download slot") } else {
                        format!("{id}: {:.0}% · {:.1} / {:.1} MB · q: cancel and go back", ratio * 100.0, first.completed as f64 / 1e6, first.total as f64 / 1e6)
                    })
                    .block(Block::bordered().title(if active.len() > 1 { format!(" Downloading ({} in queue) ", active.len()) } else { " Downloading ".into() })), progress[0]);
            } else {
                frame.render_widget(
                    Paragraph::new("No downloads in progress.").block(
                        Block::bordered().title(" Downloads ").border_style(Style::default().fg(Color::Blue)),
                    ),
                    progress[0],
                );
            }
            frame.render_widget(
                Paragraph::new(
                    active.iter().skip(1).map(|(id, progress)| {
                        if downloads.queued(id) {
                            format!("{id}: waiting for a free slot")
                        } else {
                            format!("{id}: {:.0}% · {:.1} / {:.1} MB", progress.percent(), progress.completed as f64 / 1e6, progress.total as f64 / 1e6)
                        }
                    }).collect::<Vec<_>>().join("\n"),
                )
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title(" Other downloads ")),
                progress[1],
            );
            frame.render_widget(
                Paragraph::new("hjkl / arrows: navigate · Enter: select · c: cleanup · a: retain audio · d: delete cache · ?: help · q: back to the dashboard")
                    .style(Style::default().fg(Color::Gray)),
                rows[5],
            );
            if help {
                tui::help(
                    frame,
                    "Configuration help",
                    vec![
                        tui::section("Navigation"),
                        tui::keys("j / ↓, k / ↑", "Move through models"),
                        tui::keys("g, G", "First and last model"),
                        tui::keys("h / l, ← / →", "Switch between speech and cleanup"),
                        tui::section("Actions"),
                        tui::keys("Enter", "Select and download in the background"),
                        tui::keys("d", "Delete a cached model from disk"),
                        tui::keys("c", "Enable or disable cleanup"),
                        tui::keys("a", "Toggle audio clip retention"),
                        tui::section("Downloads"),
                        tui::keys("q", "Cancel the download, return to the dashboard"),
                        tui::keys("progress", "Continues on the dashboard while it runs"),
                        tui::section("Other"),
                        tui::keys("? / Esc", "Close this help"),
                    ],
                );
            }
        })?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if help {
            match key.code {
                KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                    help = false
                }
                _ => help = true,
            }
            continue;
        }
        if key.code == KeyCode::Char('?') {
            help = true;
            continue;
        }
        if key.code == KeyCode::Char('q')
            || key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            // Downloads keep running in the background after the screen is left.
            break;
        }
        match key.code {
            KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('h')
            | KeyCode::Char('l') => {
                tab = 1 - tab;
                selection.select(Some(0));
            }
            KeyCode::Up | KeyCode::Char('k') => selection.select(Some(selected.saturating_sub(1))),
            KeyCode::Down | KeyCode::Char('j') => {
                selection.select(Some((selected + 1).min(catalog.len() - 1)))
            }
            KeyCode::Char('g') => selection.select(Some(0)),
            KeyCode::Char('G') => selection.select(Some(catalog.len() - 1)),
            KeyCode::Enter => {
                let model = catalog[selected].clone();
                let mut next = settings.clone();
                if tab == 0 {
                    next.speech_model = model.id.clone();
                } else {
                    next.filter_model = model.id.clone();
                    next.filter_enabled = true;
                }
                // Save first so a rejected change never starts a transfer.
                match runtime.block_on(service.save_settings(next)) {
                    Ok(()) => {
                        if downloads.is_installed(&model.id) {
                            message = format!("{} is cached; selection saved.", model.name);
                        } else {
                            downloads.enqueue(&model.id);
                            message = format!(
                                "{} is downloading in the background; progress continues on the dashboard.",
                                model.name
                            );
                        }
                    }
                    Err(error) => message = format!("Failed: {error:#}"),
                }
            }
            KeyCode::Char('c') => {
                let mut next = settings;
                next.filter_enabled = !next.filter_enabled;
                message = match runtime.block_on(service.save_settings(next)) {
                    Ok(()) => "Cleanup setting saved.".into(),
                    Err(error) => format!("Failed: {error:#}"),
                };
            }
            KeyCode::Char('a') => {
                let mut next = settings;
                next.retain_audio = !next.retain_audio;
                message = match runtime.block_on(service.save_settings(next)) {
                    Ok(()) => "Audio retention saved.".into(),
                    Err(error) => format!("Failed: {error:#}"),
                };
            }
            KeyCode::Char('d') => {
                let model = catalog[selected].clone();
                // Only the model that is actively selected is protected, so any
                // other cached model can be removed to free disk space.
                let selected_model = if tab == 0 {
                    model.id == settings.speech_model
                } else {
                    model.id == models::canonical_cleanup_id(&settings.filter_model)
                };
                if selected_model {
                    message = format!(
                        "{} is selected. Pick another model before deleting it.",
                        model.name
                    );
                } else if !model.installed {
                    message = format!("{} is not cached.", model.name);
                } else {
                    downloads.cancel(&model.id);
                    downloads.forget(&model.id);
                    match runtime.block_on(service.unload()) {
                        Err(error) => message = format!("Could not unload models: {error:#}"),
                        Ok(()) => match contextd_core::models::delete(&service.paths, &model.id) {
                            Ok(()) => {
                                message = format!("Deleted {} from the local cache.", model.name)
                            }
                            Err(error) => message = format!("Delete failed: {error:#}"),
                        },
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}
