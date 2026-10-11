//! Listen to voice samples and name persistent speaker profiles.
use crate::tui;
use anyhow::{Context, Result};
use contextd_core::{speakers, Paths};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use rodio::{Decoder, OutputStream, Sink};
use std::{
    fs::File,
    io::{self, BufReader, IsTerminal},
    time::Duration,
};

struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        ratatui::restore();
    }
}
struct Playback {
    _stream: OutputStream,
    sink: Sink,
}

pub fn run(paths: Paths) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        anyhow::bail!(
            "Speaker browser needs a terminal; use speakers --json or speakers name ID NAME"
        );
    }
    let mut terminal = ratatui::try_init()?;
    let _restore = Restore;
    let mut selection = ListState::default().with_selected(Some(0));
    let mut editing: Option<String> = None;
    let mut message = String::new();
    let mut playback: Option<Playback> = None;
    loop {
        let profiles = speakers::list(&paths)?;
        let selected = selection
            .selected()
            .unwrap_or(0)
            .min(profiles.len().saturating_sub(1));
        selection.select(Some(selected));
        let profile = profiles.get(selected);
        terminal.draw(|frame| {
            let rows = Layout::vertical([Constraint::Length(3),Constraint::Min(6),Constraint::Length(3),Constraint::Length(2)]).split(frame.area());
            frame.render_widget(Paragraph::new("◆ contextd / Speakers").style(Style::default().fg(tui::ACCENT).add_modifier(Modifier::BOLD)),rows[0]);
            let columns = Layout::horizontal([Constraint::Percentage(38),Constraint::Percentage(62)]).split(rows[1]);
            let items: Vec<_> = profiles.iter().map(|speaker| ListItem::new(vec![
                Line::from(speaker.name.clone()),
                Line::styled(if speaker.named { "Named voice" } else { "New voice · name needed" },Style::default().fg(tui::MUTED)),
            ])).collect();
            frame.render_stateful_widget(List::new(items).block(tui::panel(" Saved voices "))
                .highlight_style(Style::default().bg(tui::SELECTED).fg(tui::ACCENT)).highlight_symbol("› "),columns[0],&mut selection);
            let detail = profile.map(|speaker| format!("{}\n\n{}\n\nSample: {:.1} seconds\n{}\n\nEnter: listen to this voice\nn: set or change the name\n\nNaming updates earlier notes and future voice matches. Voice samples are kept separately from full recordings.",speaker.name,
                if speaker.named { "This voice is named." } else { "Listen, then enter the person's name." }, speaker.sample_seconds,speaker.sample_path))
                .unwrap_or_else(|| "No speakers yet.\n\nEnable Speaker detection in Config and record speech. A new clear voice gets a short saved sample; very brief or overlapping speech stays unknown.".into());
            frame.render_widget(Paragraph::new(detail).wrap(Wrap { trim:false }).block(tui::panel(" Identify the speaker ")),columns[1]);
            let playing = playback.as_ref().is_some_and(|p| !p.sink.empty());
            frame.render_widget(Paragraph::new(format!("{}{}",if playing { "Playing sample · " } else { "" },message)),rows[2]);
            frame.render_widget(Paragraph::new("↑/↓ j/k: select · Enter: listen · Space: pause/resume · s: stop · n: name · q/Esc: back").style(Style::default().fg(tui::MUTED)),rows[3]);
            if let Some(name) = &editing {
                let area = tui::centered(frame.area(),(65,9));
                frame.render_widget(Clear,area);
                frame.render_widget(Paragraph::new(vec![Line::from("Enter the person's name:"),Line::from(""),
                    Line::from(vec![Span::styled(name.clone(),Style::default().fg(Color::White)),Span::styled("▏",Style::default().fg(tui::ACCENT))]),
                    Line::from(""),Line::from("Enter: save and sync notes · Esc: cancel")]).block(tui::panel(" Name speaker ")),area);
            }
        })?;
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if let Some(name) = &mut editing {
            match key.code {
                KeyCode::Esc => editing = None,
                KeyCode::Backspace => {
                    name.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                    name.push(c)
                }
                KeyCode::Enter => {
                    if let Some(profile) = profile {
                        match speakers::rename(&paths, &paths.load()?, &profile.id, name) {
                            Ok(()) => {
                                message =
                                    "Name saved · earlier notes and future matches are in sync"
                                        .into();
                                editing = None;
                            }
                            Err(error) => message = format!("{error:#}"),
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Down | KeyCode::Char('j') => {
                selection.select(Some((selected + 1).min(profiles.len().saturating_sub(1))))
            }
            KeyCode::Up | KeyCode::Char('k') => selection.select(Some(selected.saturating_sub(1))),
            KeyCode::Char('n') => {
                if let Some(profile) = profile {
                    editing = Some(if profile.named {
                        profile.name.clone()
                    } else {
                        String::new()
                    });
                }
            }
            KeyCode::Enter => {
                if let Some(profile) = profile {
                    let start = || -> Result<Playback> {
                        let (stream, handle) =
                            OutputStream::try_default().context("Cannot open audio output")?;
                        let sink = Sink::try_new(&handle)?;
                        sink.append(Decoder::new(BufReader::new(File::open(
                            &profile.sample_path,
                        )?))?);
                        Ok(Playback {
                            _stream: stream,
                            sink,
                        })
                    };
                    match start() {
                        Ok(audio) => {
                            playback = Some(audio);
                            message.clear();
                        }
                        Err(error) => message = format!("Cannot play sample: {error:#}"),
                    }
                }
            }
            KeyCode::Char(' ') => {
                if let Some(audio) = &playback {
                    if audio.sink.is_paused() {
                        audio.sink.play();
                    } else {
                        audio.sink.pause();
                    }
                }
            }
            KeyCode::Char('s') => playback = None,
            _ => {}
        }
    }
}
