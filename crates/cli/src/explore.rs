use anyhow::{bail, Context, Result};
use contextd_core::{
    config::PRODUCT_NAME,
    explore::{self, Folder, SavedFile},
    Paths,
};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, List, ListItem, ListState, Paragraph, Tabs, Wrap},
    DefaultTerminal, Frame,
};
use rodio::{Decoder, OutputStream, Sink};
use std::{
    fs::File,
    io::{self, BufReader, IsTerminal},
    time::{Duration, Instant},
};

struct Playback {
    // The output stream must live until playback stops.
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

struct Browser {
    paths: Paths,
    folder: Folder,
    files: Vec<SavedFile>,
    selection: ListState,
    preview: String,
    scroll: u16,
    max_scroll: u16,
    playback: Option<Playback>,
    message: String,
}
impl Browser {
    fn refresh(&mut self) -> Result<()> {
        let selected = self
            .selection
            .selected()
            .and_then(|index| self.files.get(index))
            .cloned();
        self.files = explore::list(&self.paths, self.folder)?;
        let index = selected
            .as_ref()
            .and_then(|file| self.files.iter().position(|new| new.path == file.path))
            .or_else(|| (!self.files.is_empty()).then_some(0));
        let changed = index
            .and_then(|i| self.files.get(i))
            .map(|file| (&file.path, file.modified, file.bytes))
            != selected
                .as_ref()
                .map(|file| (&file.path, file.modified, file.bytes));
        self.selection.select(index);
        if changed || self.files.is_empty() {
            let previous_scroll = self.scroll;
            self.load_preview();
            if index.and_then(|i| self.files.get(i)).map(|file| &file.path)
                == selected.as_ref().map(|file| &file.path)
            {
                self.scroll = previous_scroll;
            }
        }
        Ok(())
    }
    fn load_preview(&mut self) {
        self.scroll = 0;
        self.preview = match self.selection.selected().and_then(|index| self.files.get(index)) {
            Some(file) if self.folder == Folder::Notes => explore::read_note(file)
                .unwrap_or_else(|error| format!("Cannot read note: {error}")),
            Some(file) => format!("{}\n\nSize: {} bytes\n\nEnter: play from the beginning\nSpace: pause / resume\ns: stop\n\nAudio is available only for clips recorded with audio retention enabled.", file.name, file.bytes),
            None if self.folder == Folder::Notes => "No saved notes yet.\n\nNotes appear here as recordings are processed or manual notes are added.".into(),
            None => "No saved audio clips.\n\nEnable retention when recording:\ncontextd --retain-audio\n\nPreviously discarded audio cannot be recovered.".into(),
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
    fn play(&mut self) {
        if self.folder != Folder::Audio {
            return;
        }
        if let Some(file) = self
            .selection
            .selected()
            .and_then(|index| self.files.get(index))
        {
            // Stop the previous clip before opening a new output stream.
            self.playback = None;
            match Playback::start(file) {
                Ok(playback) => {
                    self.playback = Some(playback);
                    self.message.clear();
                }
                Err(error) => self.message = format!("Playback failed: {error:#}"),
            }
        }
    }
    fn draw(&mut self, frame: &mut Frame) {
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
            Constraint::Length(2),
        ])
        .split(frame.area());
        frame.render_widget(
            Paragraph::new(format!("{PRODUCT_NAME} — {}", self.paths.root.display())),
            rows[0],
        );
        frame.render_widget(
            Tabs::new(["Notes", "Audio"])
                .select(if self.folder == Folder::Notes { 0 } else { 1 })
                .highlight_style(Style::default().fg(Color::Cyan))
                .block(Block::bordered().title("Tab / ← / →: switch folder")),
            rows[1],
        );
        let columns = Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)])
            .split(rows[2]);
        let items: Vec<_> = self
            .files
            .iter()
            .map(|file| ListItem::new(file.name.as_str()))
            .collect();
        frame.render_stateful_widget(
            List::new(items)
                .block(Block::bordered().title(format!("{} files", self.files.len())))
                .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
                .highlight_symbol("› "),
            columns[0],
            &mut self.selection,
        );
        let width = columns[1].width.saturating_sub(2).max(1);
        let height = usize::from(columns[1].height.saturating_sub(2));
        let preview = Paragraph::new(self.preview.as_str()).wrap(Wrap { trim: false });
        let lines = preview.line_count(width);
        self.max_scroll = lines.saturating_sub(height).min(u16::MAX as usize) as u16;
        self.scroll = self.scroll.min(self.max_scroll);
        // Preserve whitespace and wrap long transcript lines for terminal reading.
        frame.render_widget(
            preview
                .scroll((self.scroll, 0))
                .block(Block::bordered().title("Preview · PgUp / PgDn: scroll")),
            columns[1],
        );
        let status = if !self.message.is_empty() {
            self.message.clone()
        } else if let Some(playback) = &self.playback {
            format!(
                "{}: {}",
                if playback.sink.is_paused() {
                    "Paused"
                } else {
                    "Playing"
                },
                playback.name
            )
        } else {
            "Auto-refresh every 2 seconds · r: refresh now".into()
        };
        frame.render_widget(Paragraph::new(status), rows[3]);
        frame.render_widget(Paragraph::new("↑/↓ or j/k: select · Enter: play audio · Space: pause/resume · s: stop · q/Esc/Ctrl+C: quit"), rows[4]);
    }
    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        self.refresh()?;
        let mut refreshed = Instant::now();
        loop {
            if self
                .playback
                .as_ref()
                .is_some_and(|playback| playback.sink.empty())
            {
                self.playback = None;
            }
            if refreshed.elapsed() >= Duration::from_secs(2) {
                if let Err(error) = self.refresh() {
                    self.message = format!("Refresh failed: {error}");
                }
                refreshed = Instant::now();
            }
            terminal.draw(|frame| self.draw(frame))?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                break;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                    self.folder = if self.folder == Folder::Notes {
                        Folder::Audio
                    } else {
                        Folder::Notes
                    };
                    self.files.clear();
                    self.selection.select(None);
                    self.refresh()?;
                }
                KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
                KeyCode::PageDown => {
                    self.scroll = self.scroll.saturating_add(10).min(self.max_scroll)
                }
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
                KeyCode::Home => self.scroll = 0,
                KeyCode::End => self.scroll = self.max_scroll,
                KeyCode::Enter => self.play(),
                KeyCode::Char(' ') => {
                    if let Some(playback) = &self.playback {
                        if playback.sink.is_paused() {
                            playback.sink.play();
                        } else {
                            playback.sink.pause();
                        }
                    }
                }
                KeyCode::Char('s') => self.playback = None,
                KeyCode::Char('r') => {
                    self.message.clear();
                    if let Err(error) = self.refresh() {
                        self.message = format!("Refresh failed: {error}");
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
        ratatui::restore();
    }
}

pub fn run(paths: Paths) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("Explore needs an interactive terminal. Run contextd explore in your terminal.");
    }
    let _restore = RestoreTerminal;
    let mut terminal = ratatui::try_init()?;
    let mut browser = Browser {
        paths,
        folder: Folder::Notes,
        files: Vec::new(),
        selection: ListState::default(),
        preview: String::new(),
        scroll: 0,
        max_scroll: 0,
        playback: None,
        message: String::new(),
    };
    browser.event_loop(&mut terminal)
}
