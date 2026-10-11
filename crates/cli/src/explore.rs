use crate::{
    editor::{Action, Editor},
    tui,
};
use anyhow::{bail, Context, Result};
use contextd_core::{
    config::{EditorMode, PRODUCT_NAME},
    explore::{self, Folder, SavedFile},
    history::History,
    Paths, Transcript,
};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Tabs, Wrap,
    },
    DefaultTerminal, Frame,
};
use rodio::{Decoder, OutputStream, Sink};
use std::{
    fs::File,
    io::{self, BufReader, IsTerminal},
    time::{Duration, Instant},
};

struct Playback {
    _stream: OutputStream,
    sink: Sink,
    name: String,
}
impl Playback {
    fn start(file: &SavedFile) -> Result<Self> {
        let source = Decoder::new(BufReader::new(File::open(&file.path)?))?;
        let (stream, handle) = OutputStream::try_default().context("Cannot open audio output")?;
        let sink = Sink::try_new(&handle)?;
        sink.append(source);
        Ok(Self {
            _stream: stream,
            sink,
            name: file.name.clone(),
        })
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Dates,
    Preview,
}
struct Browser {
    paths: Paths,
    folder: Folder,
    files: Vec<SavedFile>,
    selection: ListState,
    preview: String,
    entries: Vec<Transcript>,
    selected_note: usize,
    note_offsets: Vec<usize>,
    scroll: usize,
    max_scroll: usize,
    page: usize,
    focus: Focus,
    editor_mode: EditorMode,
    editor: Option<Editor>,
    show_original: bool,
    playback: Option<Playback>,
    message: String,
    help: bool,
    dates_area: Rect,
    preview_area: Rect,
    tabs_area: Rect,
}
impl Browser {
    fn refresh(&mut self) -> Result<()> {
        if self.editor.is_some() {
            return Ok(());
        }
        let selected = self
            .selection
            .selected()
            .and_then(|index| self.files.get(index))
            .cloned();
        self.files = explore::list(&self.paths, self.folder)?;
        let index = selected
            .as_ref()
            .and_then(|file| {
                self.files.iter().position(|new| {
                    new.path == file.path || self.folder == Folder::Notes && new.name == file.name
                })
            })
            .or_else(|| (!self.files.is_empty()).then_some(0));
        let changed = index
            .and_then(|i| self.files.get(i))
            .map(|f| (&f.path, f.modified, f.bytes))
            != selected.as_ref().map(|f| (&f.path, f.modified, f.bytes));
        self.selection.select(index);
        if changed || self.files.is_empty() {
            let same_file = index
                .and_then(|i| self.files.get(i))
                .zip(selected.as_ref())
                .is_some_and(|(new, old)| {
                    new.path == old.path || self.folder == Folder::Notes && new.name == old.name
                });
            let previous_scroll = self.scroll;
            let note = self.selected_note;
            self.load_preview();
            if same_file {
                self.scroll = previous_scroll;
                self.selected_note = note.min(self.entries.len().saturating_sub(1));
            }
        }
        Ok(())
    }
    fn load_preview(&mut self) {
        self.scroll = 0;
        self.selected_note = 0;
        self.entries.clear();
        let file = self
            .selection
            .selected()
            .and_then(|index| self.files.get(index));
        self.preview = match file {
            Some(file) if self.folder == Folder::Notes => {
                if let Some(day) = file.path.file_stem().and_then(|s| s.to_str()).and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()) {
                    match History::open(&self.paths).and_then(|history| history.for_day(day)) {
                        Ok(entries) => self.entries = entries,
                        Err(error) => self.message = format!("Cannot read saved notes: {error:#}"),
                    }
                }
                explore::read_note(file).unwrap_or_else(|error| format!("Cannot read note: {error}"))
            }
            Some(file) => format!("{}\n\n{:.1} KB · saved audio\n\nEnter: play from the beginning\nSpace: pause / resume\ns: stop", file.name, file.bytes as f64 / 1024.0),
            None if self.folder == Folder::Notes => "Your notes will appear here.\n\nKeep recording, or add a note with contextd note.\nSelect a date to read and edit its saved notes.".into(),
            None => "No saved audio yet.\n\nEnable Keep audio recordings in Config to retain future clips.".into(),
        };
    }
    fn move_selection(&mut self, delta: isize) {
        if self.files.is_empty() {
            return;
        }
        let index = self
            .selection
            .selected()
            .unwrap_or(0)
            .saturating_add_signed(delta)
            .min(self.files.len() - 1);
        self.selection.select(Some(index));
        self.load_preview();
    }
    fn scroll_by(&mut self, delta: isize) {
        self.scroll = self
            .scroll
            .saturating_add_signed(delta)
            .min(self.max_scroll);
        if !self.note_offsets.is_empty() {
            self.selected_note = self
                .note_offsets
                .iter()
                .rposition(|offset| *offset <= self.scroll + 2)
                .unwrap_or(0);
        }
    }
    fn move_note(&mut self, delta: isize) {
        self.selected_note = self
            .selected_note
            .saturating_add_signed(delta)
            .min(self.entries.len().saturating_sub(1));
        self.scroll = self
            .note_offsets
            .get(self.selected_note)
            .copied()
            .unwrap_or(0)
            .min(self.max_scroll);
    }
    fn open_editor(&mut self) {
        if let Some(entry) = self.entries.get(self.selected_note) {
            self.editor = Some(Editor::new(
                entry.id.clone(),
                entry.refined.clone(),
                self.editor_mode,
            ));
            self.message.clear();
        } else {
            self.message = "This file has no saved notes to edit.".into();
        }
    }
    fn edit_key(&mut self, key: KeyEvent) {
        let action = self.editor.as_mut().unwrap().key(key);
        match action {
            Action::Continue => {}
            Action::Cancel => {
                self.editor = None;
                self.message = "Draft discarded · saved note unchanged".into();
            }
            Action::Save(close) => {
                let editor = self.editor.as_ref().unwrap();
                let text = editor.text();
                match History::open(&self.paths).and_then(|history| {
                    history.edit_saved_text(&editor.id, &editor.original, &text)
                }) {
                    Ok(()) => {
                        self.message = "Saved · original transcript preserved".into();
                        if close {
                            self.editor = None;
                            let selected = self.selected_note;
                            if let Err(error) = self.refresh() {
                                self.message =
                                    format!("Saved, but preview refresh failed: {error:#}");
                            }
                            self.selected_note = selected.min(self.entries.len().saturating_sub(1));
                        } else {
                            self.editor.as_mut().unwrap().original = text;
                        }
                    }
                    Err(error) => self.message = format!("Save failed: {error:#}"),
                }
            }
        }
    }
    fn play(&mut self) {
        if self.folder != Folder::Audio {
            return;
        }
        if let Some(file) = self
            .selection
            .selected()
            .and_then(|index| self.files.get(index))
        {
            self.playback = None;
            match Playback::start(file) {
                Ok(p) => {
                    self.playback = Some(p);
                    self.message.clear();
                }
                Err(e) => self.message = format!("Playback failed: {e:#}"),
            }
        }
    }
    fn switch_folder(&mut self) -> Result<()> {
        self.folder = if self.folder == Folder::Notes {
            Folder::Audio
        } else {
            Folder::Notes
        };
        self.focus = Focus::Dates;
        self.files.clear();
        self.selection.select(None);
        self.refresh()
    }
    fn preview_lines(&mut self, width: u16) -> Vec<Line<'static>> {
        self.note_offsets.clear();
        let structured = self
            .selection
            .selected()
            .and_then(|i| self.files.get(i))
            .is_some_and(|file| {
                file.path.parent() == Some(self.paths.root.join("structured").as_path())
            });
        if structured {
            let mut lines = Vec::new();
            let mut offset: usize = 0;
            for text in self.preview.lines() {
                if let Some(id) = text
                    .strip_prefix("<!-- memory-id: ")
                    .and_then(|s| s.split(';').next())
                {
                    if self
                        .entries
                        .get(self.note_offsets.len())
                        .is_some_and(|entry| entry.id == id)
                    {
                        self.note_offsets.push(offset.saturating_sub(2));
                    }
                    continue;
                }
                let line = if let Some(title) = text.strip_prefix("### ") {
                    Line::styled(
                        title.to_owned(),
                        Style::default()
                            .fg(tui::ACCENT)
                            .add_modifier(Modifier::BOLD),
                    )
                } else if let Some(title) = text.strip_prefix("## ") {
                    Line::styled(
                        title.to_owned(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                } else if let Some(title) = text.strip_prefix("# ") {
                    Line::styled(
                        title.to_owned(),
                        Style::default()
                            .fg(tui::ACCENT)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Line::from(text.to_owned())
                };
                offset += Paragraph::new(line.clone())
                    .wrap(Wrap { trim: false })
                    .line_count(width);
                lines.push(line);
            }
            // Only timestamps belonging to indexed notes participate in editing.
            self.note_offsets.truncate(self.entries.len());
            return lines;
        }
        if self.entries.is_empty() {
            return self
                .preview
                .lines()
                .map(|s| Line::from(s.to_owned()))
                .collect();
        }
        let name = self
            .selection
            .selected()
            .and_then(|i| self.files.get(i))
            .map(|f| f.name.trim_end_matches(".md"))
            .unwrap_or("");
        let title = chrono::NaiveDate::parse_from_str(name, "%Y-%m-%d")
            .map(|d| d.format("%A, %d %B %Y").to_string())
            .unwrap_or_else(|_| name.to_owned());
        let mut lines = vec![
            Line::styled(
                title,
                Style::default()
                    .fg(tui::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                format!("{} saved notes · originals retained", self.entries.len()),
                Style::default().fg(tui::MUTED),
            ),
            Line::from(""),
        ];
        let mut offset = Paragraph::new(lines.clone())
            .wrap(Wrap { trim: false })
            .line_count(width);
        for (index, entry) in self.entries.iter().enumerate() {
            self.note_offsets.push(offset);
            let mut note = Vec::new();
            let time = chrono::DateTime::parse_from_rfc3339(&entry.created_at)
                .map(|t| {
                    t.with_timezone(&chrono::Local)
                        .format("%-I:%M %p")
                        .to_string()
                })
                .unwrap_or_default();
            let selected = index == self.selected_note && self.focus == Focus::Preview;
            note.push(Line::styled(
                format!(
                    "{}  {}  ·  NOTE {:02}",
                    if selected { "●" } else { "○" },
                    time,
                    index + 1
                ),
                Style::default()
                    .fg(if selected { tui::ACCENT } else { tui::MUTED })
                    .add_modifier(Modifier::BOLD),
            ));
            note.push(Line::from(""));
            note.extend(
                entry
                    .refined
                    .lines()
                    .map(|line| Line::from(line.to_owned())),
            );
            for turn in &entry.speaker_turns {
                note.push(Line::styled(
                    format!(
                        "{} · +{:.1}s{}",
                        turn.speaker_name,
                        turn.start_seconds,
                        if turn.overlap {
                            " · overlapping speech"
                        } else {
                            ""
                        }
                    ),
                    Style::default().fg(tui::ACCENT),
                ));
                if entry.speaker_turns.len() > 1 {
                    note.extend(turn.refined.lines().map(|line| Line::from(line.to_owned())));
                }
            }
            if let Some(source) = &entry.microphone {
                note.push(Line::styled(
                    format!("Source · {source}"),
                    Style::default().fg(tui::MUTED),
                ));
            }
            if self.show_original && entry.raw != entry.refined {
                note.push(Line::styled(
                    "Original transcript",
                    Style::default().fg(Color::Yellow),
                ));
                note.extend(
                    entry
                        .raw
                        .lines()
                        .map(|line| Line::styled(line.to_owned(), Style::default().fg(tui::MUTED))),
                );
            }
            note.push(Line::from(""));
            note.push(Line::styled(
                "─".repeat(usize::from(width.min(48))),
                Style::default().fg(Color::Rgb(49, 66, 84)),
            ));
            note.push(Line::from(""));
            offset += Paragraph::new(note.clone())
                .wrap(Wrap { trim: false })
                .line_count(width);
            lines.extend(note);
        }
        lines
    }
    fn draw(&mut self, frame: &mut Frame) {
        let rows = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(2),
            Constraint::Length(2),
        ])
        .split(frame.area());
        frame.render_widget(
            Block::default().style(Style::default().bg(tui::BACKGROUND)),
            frame.area(),
        );
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(
                        format!(" ◆ {PRODUCT_NAME}  "),
                        Style::default()
                            .fg(tui::ACCENT)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "Your notes, in your words",
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::styled(
                    self.paths.root.display().to_string(),
                    Style::default().fg(tui::MUTED),
                ),
            ]),
            rows[0],
        );
        self.tabs_area = rows[1];
        frame.render_widget(
            Tabs::new(["1  Notes", "2  Audio"])
                .select(if self.folder == Folder::Notes { 0 } else { 1 })
                .highlight_style(
                    Style::default()
                        .fg(tui::ACCENT)
                        .bg(tui::SELECTED)
                        .add_modifier(Modifier::BOLD),
                )
                .block(tui::panel(" Library · 1 / 2: switch ")),
            rows[1],
        );
        let columns = Layout::horizontal([
            Constraint::Length(frame.area().width.clamp(18, 32)),
            Constraint::Min(12),
        ])
        .split(rows[2]);
        self.dates_area = columns[0];
        self.preview_area = columns[1];
        let items: Vec<_> = self
            .files
            .iter()
            .map(|f| {
                ListItem::new(vec![
                    Line::styled(
                        f.name.trim_end_matches(".md").to_owned(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Line::styled(
                        format!("  {:.1} KB", f.bytes as f64 / 1024.0),
                        Style::default().fg(tui::MUTED),
                    ),
                ])
            })
            .collect();
        frame.render_stateful_widget(
            List::new(items)
                .block(
                    tui::panel(format!(
                        " {} · {} ",
                        if self.folder == Folder::Notes {
                            "Dates"
                        } else {
                            "Clips"
                        },
                        self.files.len()
                    ))
                    .border_style(Style::default().fg(
                        if self.focus == Focus::Dates {
                            tui::ACCENT
                        } else {
                            tui::MUTED
                        },
                    )),
                )
                .highlight_style(Style::default().bg(tui::SELECTED).fg(Color::White))
                .highlight_symbol("› "),
            columns[0],
            &mut self.selection,
        );
        let block = tui::panel(if self.editor.is_some() {
            " Edit saved note "
        } else if self.focus == Focus::Preview {
            " Preview · focused "
        } else {
            " Preview "
        })
        .border_style(Style::default().fg(if self.focus == Focus::Preview {
            tui::ACCENT
        } else {
            tui::MUTED
        }));
        let inner = block.inner(columns[1]);
        frame.render_widget(block, columns[1]);
        let content = Rect::new(
            inner.x.saturating_add(2),
            inner.y.saturating_add(1),
            inner.width.saturating_sub(4),
            inner.height.saturating_sub(2),
        );
        let width = content.width.max(1);
        self.page = usize::from(content.height.max(1));
        if let Some(editor) = &mut self.editor {
            editor.width = usize::from(width);
            let (lines, row, col) = editor.visual_lines(usize::from(width));
            if row < editor.scroll {
                editor.scroll = row;
            }
            if row >= editor.scroll + self.page {
                editor.scroll = row + 1 - self.page;
            }
            frame.render_widget(
                Paragraph::new(lines.join("\n"))
                    .scroll((editor.scroll.min(u16::MAX as usize) as u16, 0)),
                content,
            );
            if content.height > 0 && content.width > 0 {
                frame.set_cursor_position((
                    content.x + (col as u16).min(width - 1),
                    content.y + (row - editor.scroll) as u16,
                ));
            }
        } else {
            let lines = self.preview_lines(width);
            let preview = Paragraph::new(lines).wrap(Wrap { trim: false });
            let count = preview.line_count(width);
            self.max_scroll = count.saturating_sub(self.page);
            self.scroll = self.scroll.min(self.max_scroll);
            frame.render_widget(
                preview.scroll((self.scroll.min(u16::MAX as usize) as u16, 0)),
                content,
            );
            let mut state = ScrollbarState::new(count)
                .position(self.scroll)
                .viewport_content_length(self.page);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                inner,
                &mut state,
            );
        }
        let status = if let Some(editor) = &self.editor {
            format!(
                "{}{} · line {}:{} · {}",
                if editor.mode == EditorMode::Normal {
                    "NORMAL EDIT"
                } else if editor.insert {
                    "VIM INSERT"
                } else {
                    "VIM NORMAL"
                },
                if editor.dirty() { " · unsaved" } else { "" },
                editor.row + 1,
                editor.column + 1,
                self.message
            )
        } else if !self.message.is_empty() {
            self.message.clone()
        } else if let Some(p) = &self.playback {
            format!(
                "{} · {}",
                if p.sink.is_paused() {
                    "Paused"
                } else {
                    "Playing"
                },
                p.name
            )
        } else {
            format!(
                "{} · note {} / {} · {} / {} lines · auto-refresh",
                if self.focus == Focus::Dates {
                    "Select a date, then Enter or click to read"
                } else {
                    "Preview focused"
                },
                if self.entries.is_empty() {
                    0
                } else {
                    self.selected_note + 1
                },
                self.entries.len(),
                self.scroll + 1,
                self.max_scroll + self.page
            )
        };
        frame.render_widget(
            Paragraph::new(status)
                .style(Style::default().fg(tui::ACCENT))
                .wrap(Wrap { trim: false }),
            rows[3],
        );
        let hints = if let Some(editor) = &self.editor {
            if let Some(command) = &editor.command {
                format!(":{command}")
            } else if editor.mode == EditorMode::Vim {
                "h/j/k/l, w/b · i/a/o: insert · Esc: normal · :w/:wq or Ctrl+s: save · q: discard"
                    .into()
            } else {
                "Arrows: move · type to edit · Ctrl+s: save · Esc: discard".into()
            }
        } else {
            "↑/↓ j/k: move · Tab: focus · Enter/e: edit note · n/p: note · PgUp/PgDn: scroll · o: original · ?: help · q: back".into()
        };
        frame.render_widget(
            Paragraph::new(hints)
                .style(Style::default().fg(tui::MUTED))
                .wrap(Wrap { trim: false }),
            rows[4],
        );
        if self.help {
            tui::help(
                frame,
                "Explore help",
                vec![
                    tui::section("Read and edit"),
                    tui::keys("Enter / click a date", "Focus the preview"),
                    tui::keys("↑/↓, j/k, mouse wheel", "Scroll the focused preview"),
                    tui::keys("Tab / Shift+Tab", "Move focus between dates and preview"),
                    tui::keys("PgUp/PgDn, g/G", "Page / first / last line"),
                    tui::keys("n / p", "Select the next / previous note"),
                    tui::keys("e / Enter", "Edit the selected saved note"),
                    tui::keys("o", "Show / hide original transcripts"),
                    tui::keys("Ctrl+s", "Save edits; preserve the original"),
                    tui::keys("Config → Editor controls", "Choose Normal or Vim editing"),
                    tui::section("Audio"),
                    tui::keys("1 / 2", "Notes / Audio"),
                    tui::keys("Enter, Space, s", "Play, pause/resume, stop"),
                    tui::keys("Esc / q", "Back to dates, then leave Explore"),
                ],
            );
        }
    }
    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        self.refresh()?;
        let mut refreshed = Instant::now();
        loop {
            if self.playback.as_ref().is_some_and(|p| p.sink.empty()) {
                self.playback = None;
            }
            if refreshed.elapsed() >= Duration::from_secs(2) && self.editor.is_none() {
                if let Err(e) = self.refresh() {
                    self.message = format!("Refresh failed: {e:#}");
                }
                refreshed = Instant::now();
            }
            terminal.draw(|frame| self.draw(frame))?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            match event::read()? {
                Event::Mouse(mouse) if self.editor.is_none() && !self.help => match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left)
                        if self.dates_area.contains((mouse.column, mouse.row).into()) =>
                    {
                        let offset =
                            usize::from(mouse.row.saturating_sub(self.dates_area.y + 1)) / 2;
                        let index = self.selection.offset() + offset;
                        if index < self.files.len() {
                            self.selection.select(Some(index));
                            self.load_preview();
                            self.focus = Focus::Preview;
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left)
                        if self.preview_area.contains((mouse.column, mouse.row).into()) =>
                    {
                        self.focus = Focus::Preview;
                        let line = self.scroll
                            + usize::from(mouse.row.saturating_sub(self.preview_area.y + 2));
                        self.selected_note = self
                            .note_offsets
                            .iter()
                            .rposition(|offset| *offset <= line)
                            .unwrap_or(0);
                    }
                    MouseEventKind::ScrollDown => {
                        if self.preview_area.contains((mouse.column, mouse.row).into())
                            || self.focus == Focus::Preview
                        {
                            self.scroll_by(3)
                        } else {
                            self.move_selection(1)
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        if self.preview_area.contains((mouse.column, mouse.row).into())
                            || self.focus == Focus::Preview
                        {
                            self.scroll_by(-3)
                        } else {
                            self.move_selection(-1)
                        }
                    }
                    _ => {}
                },
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if self.editor.is_some() {
                        self.edit_key(key);
                        continue;
                    }
                    if self.help {
                        if matches!(
                            key.code,
                            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
                        ) {
                            self.help = false
                        }
                        continue;
                    }
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break;
                    }
                    match key.code {
                        KeyCode::Char('?') => self.help = true,
                        KeyCode::Char('q') | KeyCode::Esc if self.focus == Focus::Preview => {
                            self.focus = Focus::Dates
                        }
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Tab | KeyCode::BackTab => {
                            self.focus = if self.focus == Focus::Dates {
                                Focus::Preview
                            } else {
                                Focus::Dates
                            }
                        }
                        KeyCode::Char('1') if self.folder != Folder::Notes => {
                            self.switch_folder()?
                        }
                        KeyCode::Char('2') if self.folder != Folder::Audio => {
                            self.switch_folder()?
                        }
                        KeyCode::Left if self.focus == Focus::Dates => self.switch_folder()?,
                        KeyCode::Right if self.focus == Focus::Dates => self.focus = Focus::Preview,
                        KeyCode::Up | KeyCode::Char('k') => {
                            if self.focus == Focus::Preview {
                                self.scroll_by(-1)
                            } else {
                                self.move_selection(-1)
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if self.focus == Focus::Preview {
                                self.scroll_by(1)
                            } else {
                                self.move_selection(1)
                            }
                        }
                        KeyCode::PageDown => self.scroll_by(self.page as isize),
                        KeyCode::PageUp => self.scroll_by(-(self.page as isize)),
                        KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
                        KeyCode::End | KeyCode::Char('G') => self.scroll = self.max_scroll,
                        KeyCode::Char('n') if self.focus == Focus::Preview => self.move_note(1),
                        KeyCode::Char('p') if self.focus == Focus::Preview => self.move_note(-1),
                        KeyCode::Char('e') if self.folder == Folder::Notes => {
                            self.focus = Focus::Preview;
                            self.open_editor();
                        }
                        KeyCode::Enter if self.folder == Folder::Audio => self.play(),
                        KeyCode::Enter if self.focus == Focus::Dates => self.focus = Focus::Preview,
                        KeyCode::Enter => self.open_editor(),
                        KeyCode::Char('o') => self.show_original = !self.show_original,
                        KeyCode::Char(' ') => {
                            if let Some(p) = &self.playback {
                                if p.sink.is_paused() {
                                    p.sink.play()
                                } else {
                                    p.sink.pause()
                                }
                            }
                        }
                        KeyCode::Char('s') => self.playback = None,
                        KeyCode::Char('r') => {
                            self.message.clear();
                            if let Err(e) = self.refresh() {
                                self.message = format!("Refresh failed: {e:#}")
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}
struct RestoreTerminal;
impl Drop for RestoreTerminal {
    fn drop(&mut self) {
        let _ = crossterm::execute!(io::stdout(), DisableMouseCapture);
        ratatui::restore();
    }
}
pub fn run(paths: Paths) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("Explore needs an interactive terminal. Run contextd explore in your terminal.");
    }
    let editor_mode = paths.load()?.editor_mode;
    let _restore = RestoreTerminal;
    let mut terminal = ratatui::try_init()?;
    crossterm::execute!(io::stdout(), EnableMouseCapture)?;
    let mut browser = Browser {
        paths,
        folder: Folder::Notes,
        files: Vec::new(),
        selection: ListState::default(),
        preview: String::new(),
        entries: Vec::new(),
        selected_note: 0,
        note_offsets: Vec::new(),
        scroll: 0,
        max_scroll: 0,
        page: 1,
        focus: Focus::Dates,
        editor_mode,
        editor: None,
        show_original: false,
        playback: None,
        message: String::new(),
        help: false,
        dates_area: Rect::default(),
        preview_area: Rect::default(),
        tabs_area: Rect::default(),
    };
    browser.event_loop(&mut terminal)
}
