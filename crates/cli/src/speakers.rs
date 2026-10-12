//! Listen to voice samples and name persistent speaker profiles.
use crate::tui;
use anyhow::{Context, Result};
use contextd_core::{history::History, speakers, Paths};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use rodio::{Decoder, OutputStream, Sink, Source};
use std::{
    fs::File,
    io::{self, BufReader, IsTerminal},
    sync::mpsc::{self, Receiver, TryRecvError},
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

// Never run history rewrites or file synchronization on the terminal thread.
// Dropping the receiver on exit does not wait for the worker.
fn background_edit(
    operation: impl FnOnce() -> Result<String> + Send + 'static,
) -> Receiver<Result<String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(operation());
    });
    receiver
}

fn profiles(paths: &Paths) -> Result<Vec<speakers::Speaker>> {
    let mut profiles = speakers::list(paths)?;
    profiles.extend(speakers::unknown_turns(paths)?);
    Ok(profiles)
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
    let mut linking: Option<usize> = None;
    let mut deleting: Option<Vec<String>> = None;
    let mut message = String::new();
    let mut playback: Option<Playback> = None;
    let mut pending: Option<Receiver<Result<String>>> = None;
    let mut profiles = profiles(&paths)?;
    loop {
        if let Some(worker) = &pending {
            match worker.try_recv() {
                Ok(result) => {
                    message = match result {
                        Ok(message) => message,
                        Err(error) => format!("{error:#}"),
                    };
                    pending = None;
                    // Even a sync failure may have committed the identity change.
                    profiles = self::profiles(&paths)?;
                }
                Err(TryRecvError::Disconnected) => {
                    pending = None;
                    message = "Speaker update worker stopped; reopen Speakers to refresh".into();
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        let selected = selection
            .selected()
            .unwrap_or(0)
            .min(profiles.len().saturating_sub(1));
        selection.select(Some(selected));
        let profile = profiles.get(selected);
        let targets: Vec<_> = profiles
            .iter()
            .filter(|p| {
                !p.id.starts_with("unknown:") && profile.is_none_or(|source| source.id != p.id)
            })
            .collect();
        terminal.draw(|frame| {
            let rows = Layout::vertical([Constraint::Length(3),Constraint::Min(6),Constraint::Length(3),Constraint::Length(2)]).split(frame.area());
            frame.render_widget(Paragraph::new("◆ contextd / Speakers").style(Style::default().fg(tui::ACCENT).add_modifier(Modifier::BOLD)),rows[0]);
            let columns = Layout::horizontal([Constraint::Percentage(38),Constraint::Percentage(62)]).split(rows[1]);
            let items: Vec<_> = profiles.iter().map(|speaker| ListItem::new(vec![
                Line::from(speaker.name.clone()),
                Line::styled(if speaker.named { "Named voice" } else { "Unnamed / unknown" },Style::default().fg(tui::MUTED)),
            ])).collect();
            frame.render_stateful_widget(List::new(items).block(tui::panel(" Saved voices "))
                .highlight_style(Style::default().bg(tui::SELECTED).fg(tui::ACCENT)).highlight_symbol("› "),columns[0],&mut selection);
            let detail = profile.map(|speaker| format!("{}\n\n{}\n\nSample: {:.1} seconds\n{}\n\nEnter: listen to this voice\nn: set or change the name\nl: link to an existing speaker\nd: delete this entry · D: delete all unnamed\n\nNaming updates earlier notes and future voice matches. Voice samples are kept separately from full recordings.",speaker.name,
                if speaker.id.starts_with("unknown:") {
                    if speaker.sample_path.is_empty() { "Original recording was not retained." } else { "Unknown turn from the retained recording." }
                } else if speaker.named { "This voice is named." } else { "Listen, then enter the person's name." }, speaker.sample_seconds,speaker.sample_path))
                .unwrap_or_else(|| "No speakers yet.\n\nEnable Speaker detection in Config and record speech. A new clear voice gets a short saved sample; very brief or overlapping speech stays unknown.".into());
            frame.render_widget(Paragraph::new(detail).wrap(Wrap { trim:false }).block(tui::panel(" Identify the speaker ")),columns[1]);
            let playing = playback.as_ref().is_some_and(|p| !p.sink.empty());
            frame.render_widget(Paragraph::new(format!("{}{}",if playing { "Playing sample · " } else { "" },message)),rows[2]);
            frame.render_widget(Paragraph::new("↑/↓ j/k: select · Enter: listen · Space: pause/resume · s: stop · n: name · l: link · d: delete · D: delete unnamed · q/Esc: back").style(Style::default().fg(tui::MUTED)),rows[3]);
            if let Some(index) = linking {
                let area = tui::centered(frame.area(), (70, 18));
                frame.render_widget(Clear, area);
                let items: Vec<_> = targets.iter().map(|p| ListItem::new(p.name.clone())).collect();
                let mut state = ListState::default().with_selected(Some(index));
                frame.render_stateful_widget(List::new(items).block(tui::panel(" Link to speaker · ↑/↓ select · Enter save · Esc cancel ")).highlight_style(Style::default().bg(tui::SELECTED).fg(tui::ACCENT)).highlight_symbol("› "), area, &mut state);
            }
            if let Some(ids) = &deleting {
                let area = tui::centered(frame.area(), (65, 9));
                frame.render_widget(Clear, area);
                frame.render_widget(Paragraph::new(format!("Delete {} speaker entry(s)?\n\nSaved voices will no longer be matched.\nUnknown entries will leave this list.\nRecorded text and earlier names are preserved.\n\ny: delete · Esc: cancel", ids.len())).block(tui::panel(" Delete speaker ")), area);
            }
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
        if pending.is_some() && !matches!(key.code, KeyCode::Char('q' | ' ' | 's') | KeyCode::Esc) {
            continue;
        }
        if let Some(ids) = &deleting {
            match key.code {
                KeyCode::Esc | KeyCode::Char('n') => deleting = None,
                KeyCode::Char('y') => {
                    playback = None;
                    match speakers::delete(&paths, ids) {
                        Ok(()) => {
                            message = format!("Deleted {} entry(s)", ids.len());
                            deleting = None;
                            profiles = self::profiles(&paths)?;
                        }
                        Err(error) => message = format!("{error:#}"),
                    }
                }
                _ => {}
            }
            continue;
        }
        if let Some(index) = linking {
            match key.code {
                KeyCode::Esc => linking = None,
                KeyCode::Down | KeyCode::Char('j') => {
                    linking = Some((index + 1).min(targets.len().saturating_sub(1)))
                }
                KeyCode::Up | KeyCode::Char('k') => linking = Some(index.saturating_sub(1)),
                KeyCode::Enter => {
                    if let (Some(source), Some(target)) = (profile, targets.get(index)) {
                        let paths = paths.clone();
                        let source = source.id.clone();
                        let target = (*target).clone();
                        pending = Some(background_edit(move || {
                            speakers::link(&paths, &paths.load()?, &source, &target.id)?;
                            Ok(format!("Linked to {} · notes synchronized", target.name))
                        }));
                        message = "Linking speaker and synchronizing notes… · q/Esc: back".into();
                        linking = None;
                    }
                }
                _ => {}
            }
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
                        let paths = paths.clone();
                        let id = profile.id.clone();
                        let name = name.clone();
                        pending = Some(background_edit(move || {
                            speakers::rename(&paths, &paths.load()?, &id, &name)?;
                            Ok("Name saved · earlier notes and future matches are in sync".into())
                        }));
                        message = "Saving speaker and synchronizing notes… · q/Esc: back".into();
                        editing = None;
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
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(profile) = profile {
                    deleting = Some(vec![profile.id.clone()]);
                }
            }
            KeyCode::Char('D') => {
                let ids: Vec<_> = profiles
                    .iter()
                    .filter(|p| !p.named)
                    .map(|p| p.id.clone())
                    .collect();
                if !ids.is_empty() {
                    deleting = Some(ids);
                }
            }
            KeyCode::Char('l') => {
                if profile.is_some() && !targets.is_empty() {
                    linking = Some(0);
                }
            }
            KeyCode::Char('n') => {
                if let Some(profile) = profile.filter(|p| !p.id.starts_with("unknown:")) {
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
                        let (sample_path, start_seconds, duration_seconds) =
                            if let Some(value) = profile.id.strip_prefix("unknown:") {
                                let (transcript_id, index) = value
                                    .rsplit_once(':')
                                    .context("Invalid unknown speaker turn")?;
                                let index: usize = index.parse()?;
                                let entry = History::open(&paths)?
                                    .list(10000)?
                                    .into_iter()
                                    .find(|entry| entry.id == transcript_id)
                                    .context("Transcript not found")?;
                                let turn = entry
                                    .speaker_turns
                                    .get(index)
                                    .context("Speaker turn not found")?;
                                let sample_path = entry
                                    .audio_path
                                    .context("Original recording was not retained")?;
                                (
                                    sample_path,
                                    turn.start_seconds,
                                    turn.end_seconds - turn.start_seconds,
                                )
                            } else {
                                if profile.sample_path.is_empty() {
                                    anyhow::bail!("This voice has no playable sample");
                                }
                                (profile.sample_path.clone(), 0.0, profile.sample_seconds)
                            };
                        let source = Decoder::new(BufReader::new(File::open(sample_path)?))?
                            .skip_duration(Duration::from_secs_f64(start_seconds.max(0.0)))
                            .take_duration(Duration::from_secs_f64(duration_seconds.max(0.0)));
                        let (stream, handle) =
                            OutputStream::try_default().context("Cannot open audio output")?;
                        let sink = Sink::try_new(&handle)?;
                        sink.append(source);
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
                        Err(error) => message = format!("Cannot play speaker audio: {error:#}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaker_edit_does_not_block_input_or_wait_on_exit() {
        let (release, wait) = mpsc::channel();
        let worker = background_edit(move || {
            wait.recv()?;
            Ok("Saved".into())
        });
        assert!(matches!(worker.try_recv(), Err(TryRecvError::Empty)));
        // Closing the browser is safe while synchronization is still running.
        drop(worker);
        release.send(()).unwrap();
    }
}
