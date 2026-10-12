use crate::{downloads::Downloads, tui};
use anyhow::{bail, Result};
use contextd_core::{models, Event as ServiceEvent, Service, Settings};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Axis, Block, Chart, Dataset, Gauge, GraphType, List, ListItem, ListState, Paragraph, Wrap,
    },
};
use std::{
    collections::VecDeque,
    io::{self, IsTerminal},
    sync::Arc,
    time::{Duration, Instant},
};

/// Process-tree resident memory includes the model workers, not the whole machine.
struct Usage {
    system: sysinfo::System,
    last: Instant,
    cpu: VecDeque<u64>,
    memory: VecDeque<u64>,
    peak: u64,
}
impl Usage {
    fn new() -> Self {
        let mut value = Self {
            system: sysinfo::System::new(),
            last: Instant::now(),
            cpu: VecDeque::new(),
            memory: VecDeque::new(),
            peak: 0,
        };
        value.refresh();
        value
    }
    fn refresh(&mut self) {
        self.system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            sysinfo::ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory(),
        );
        let mut pids =
            std::collections::HashSet::from([sysinfo::Pid::from_u32(std::process::id())]);
        loop {
            let before = pids.len();
            for (pid, process) in self.system.processes() {
                if process
                    .parent()
                    .is_some_and(|parent| pids.contains(&parent))
                {
                    pids.insert(*pid);
                }
            }
            if pids.len() == before {
                break;
            }
        }
        let mut cpu = 0.0f32;
        let mut bytes = 0u64;
        for pid in pids {
            if let Some(process) = self.system.process(pid) {
                cpu += process.cpu_usage();
                bytes = bytes.saturating_add(process.memory());
            }
        }
        let mib = bytes / (1024 * 1024);
        self.peak = self.peak.max(mib);
        self.cpu.push_back(cpu.round() as u64);
        self.memory.push_back(mib);
        if self.cpu.len() > 60 {
            self.cpu.pop_front();
            self.memory.pop_front();
        }
        self.last = Instant::now();
    }
    fn render(&self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect) {
        let columns = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area);
        let cpu: Vec<_> = self
            .cpu
            .iter()
            .enumerate()
            .map(|(i, value)| {
                (
                    i as f64 - self.cpu.len().saturating_sub(1) as f64,
                    *value as f64,
                )
            })
            .collect();
        let memory: Vec<_> = self
            .memory
            .iter()
            .enumerate()
            .map(|(i, value)| {
                (
                    i as f64 - self.memory.len().saturating_sub(1) as f64,
                    *value as f64,
                )
            })
            .collect();
        let cpu_max = self.cpu.iter().copied().max().unwrap_or(100).max(100);
        for (area, data, max, title, color, unit) in [
            (
                columns[0],
                &cpu,
                cpu_max,
                format!(" CPU {}% ", self.cpu.back().unwrap_or(&0)),
                Color::Cyan,
                "%",
            ),
            (
                columns[1],
                &memory,
                self.peak.max(1),
                format!(
                    " RAM {} MiB · peak {} ",
                    self.memory.back().unwrap_or(&0),
                    self.peak
                ),
                Color::Green,
                "MiB",
            ),
        ] {
            frame.render_widget(
                Chart::new(vec![Dataset::default()
                    .data(data)
                    .graph_type(GraphType::Line)
                    .marker(ratatui::symbols::Marker::Braille)
                    .style(Style::default().fg(color))])
                .block(tui::panel(title))
                .x_axis(
                    Axis::default()
                        .bounds([-59.0, 0.0])
                        .labels(["-59s", "-30s", "now"])
                        .style(Style::default().fg(tui::MUTED)),
                )
                .y_axis(
                    Axis::default()
                        .bounds([0.0, max as f64])
                        .labels(["0".to_string(), format!("{max}{unit}")])
                        .style(Style::default().fg(tui::MUTED)),
                ),
                area,
            );
        }
    }
}

struct RestoreTerminal;
impl Drop for RestoreTerminal {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

#[derive(Clone, Copy)]
enum Job {
    Start,
    Stop,
    Config,
    Setup,
    Exit,
}

struct Pending {
    task: tokio::task::JoinHandle<Result<()>>,
    job: Job,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Pending {
    /// Cancel the task and let its cancellation finish before anything else runs.
    fn join(&mut self, runtime: &tokio::runtime::Handle) {
        // Swapping keeps the handle owned while the task is dropped.
        let task = std::mem::replace(&mut self.task, runtime.spawn(async { Ok(()) }));
        task.abort();
        let _ = runtime.block_on(task);
    }
}

/// Ready the selected models through the shared download queue, then capture.
/// Background downloads started elsewhere simply resume here instead of restarting.
fn start(
    service: &Arc<Service>,
    runtime: &tokio::runtime::Handle,
    downloads: &Arc<Downloads>,
    settings: Settings,
) -> Pending {
    let service = service.clone();
    let downloads = downloads.clone();
    Pending {
        job: Job::Start,
        task: runtime.spawn(async move {
            let mut required = vec![settings.speech_model.clone()];
            if settings.filter_enabled {
                required.push(settings.filter_model.clone());
            }
            for id in required {
                downloads.wait(&id).await?;
            }
            service.save_settings(settings).await?;
            service.start().await
        }),
    }
}

fn stop_task(
    service: &Arc<Service>,
    runtime: &tokio::runtime::Handle,
) -> tokio::task::JoinHandle<Result<()>> {
    let service = service.clone();
    runtime.spawn(async move { service.stop().await })
}

/// Abandon in-flight downloads and stop the microphone before leaving the dashboard.
fn stop_all(
    service: &Arc<Service>,
    runtime: &tokio::runtime::Handle,
    downloads: &Arc<Downloads>,
    pending: Option<Pending>,
) -> Pending {
    if let Some(mut active) = pending {
        active.join(runtime);
    }
    downloads.cancel_all();
    Pending {
        job: Job::Exit,
        task: stop_task(service, runtime),
    }
}

fn log(logs: &mut VecDeque<Line<'static>>, color: Color, message: String) {
    logs.push_back(Line::from(Span::styled(
        format!("{}  {message}", chrono::Local::now().format("%H:%M:%S")),
        Style::default().fg(color),
    )));
    while logs.len() > 400 {
        logs.pop_front();
    }
}

/// One log line per percent of a download keeps the panel in step with the gauge.
/// Returns the message and the percent it stands for, so repeats are skipped.
fn download_message(id: &str, completed: u64, total: u64) -> Option<(String, u64)> {
    if total == 0 {
        return None;
    }
    let percent = (completed.saturating_mul(100) / total).min(100);
    Some((
        if percent >= 100 {
            format!(
                "Downloaded {id}: {:.1} MB installed",
                completed as f64 / 1e6
            )
        } else {
            format!(
                "Downloading {id}: {percent}% · {:.1} / {:.1} MB",
                completed as f64 / 1e6,
                total as f64 / 1e6
            )
        },
        percent,
    ))
}

pub fn run(
    service: Arc<Service>,
    runtime: tokio::runtime::Handle,
    downloads: Arc<Downloads>,
    first_run: bool,
    retain_audio: Option<bool>,
) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("contextd needs an interactive terminal. Run contextd in your terminal to open the recording dashboard.");
    }
    let _restore = RestoreTerminal;
    let mut terminal = ratatui::try_init()?;
    let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shutdown_flag = shutdown.clone();
    let _signal = Pending {
        job: Job::Exit,
        task: runtime.spawn(async move {
            crate::shutdown_signal().await?;
            shutdown_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }),
    };
    let mut selection = ListState::default().with_selected(Some(0));
    let mut events = service.events.subscribe();
    let mut settings = service.settings();
    if first_run {
        settings.filter_enabled = true;
    }
    if let Some(retain) = retain_audio {
        settings.retain_audio = retain;
    }
    let mut setup = first_run;
    let mut logs = VecDeque::new();
    let mut pending = if setup {
        None
    } else {
        Some(start(&service, &runtime, &downloads, settings.clone()))
    };
    let mut recording = false;
    let mut processing = false;
    let mut level = 0.0f32;
    let mut logged_download = (String::new(), 101u64);
    let mut scroll = 0u16;
    let mut help = false;
    let mut usage = Usage::new();
    log(
        &mut logs,
        Color::Cyan,
        if setup {
            "Welcome! Complete setup to begin recording."
        } else {
            "Preparing models and starting your microphone…"
        }
        .into(),
    );
    loop {
        if usage.last.elapsed() >= Duration::from_secs(1) {
            usage.refresh();
        }
        if shutdown.load(std::sync::atomic::Ordering::Relaxed)
            && !pending.as_ref().is_some_and(|p| matches!(p.job, Job::Exit))
        {
            pending = Some(stop_all(&service, &runtime, &downloads, pending.take()));
        }
        loop {
            match events.try_recv() {
                Ok(ServiceEvent::Status {
                    recording: r,
                    processing: p,
                }) => {
                    recording = r;
                    processing = p;
                }
                Ok(ServiceEvent::Level { value }) => level = value,
                Ok(ServiceEvent::Download {
                    id,
                    completed,
                    total,
                }) => {
                    if let Some((message, percent)) = download_message(&id, completed, total) {
                        if logged_download != (id.clone(), percent) {
                            log(&mut logs, Color::Cyan, message);
                            logged_download = (id, percent);
                        }
                    }
                }
                Ok(ServiceEvent::Activity { log: entry }) => {
                    log(&mut logs, Color::Gray, entry.message)
                }
                Ok(ServiceEvent::Error { message }) => log(&mut logs, Color::Red, message),
                Ok(ServiceEvent::Processed { log: entry }) => {
                    if !entry.speakers.is_empty() {
                        log(
                            &mut logs,
                            Color::Cyan,
                            format!("Speakers: {}", entry.speakers.join(", ")),
                        );
                    }
                    log(
                        &mut logs,
                        Color::Gray,
                        format!("Speech started: {}", entry.started_at),
                    );
                    log(
                        &mut logs,
                        Color::Cyan,
                        format!("Original: {}", entry.raw_text),
                    );
                    log(
                        &mut logs,
                        Color::Green,
                        format!("Saved: {}", entry.saved_text),
                    );
                    log(&mut logs, Color::Gray, format!("Note: {}", entry.note_path));
                    if let Some(path) = entry.audio_path {
                        log(&mut logs, Color::Gray, format!("Audio: {path}"));
                    }
                    if let Some(error) = entry.filter_error {
                        log(&mut logs, Color::Yellow, format!("Cleanup failed: {error}"));
                    }
                }
                Ok(ServiceEvent::Unloaded) => {
                    log(&mut logs, Color::Gray, "Idle models unloaded.".into())
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(count)) => log(
                    &mut logs,
                    Color::Yellow,
                    format!("Skipped {count} live events; durable logs remain on disk."),
                ),
                Err(_) => break,
            }
        }
        if pending.as_ref().is_some_and(|p| p.task.is_finished()) {
            let mut completed = pending.take().unwrap();
            let result = runtime.block_on(&mut completed.task);
            match result {
                Ok(Ok(())) => match completed.job {
                    Job::Start => {
                        recording = true;
                        log(
                            &mut logs,
                            Color::Green,
                            "Listening — speech will be processed automatically.".into(),
                        );
                    }
                    Job::Stop => {
                        recording = false;
                        log(&mut logs, Color::Yellow, "Recording paused.".into());
                    }
                    Job::Config => {
                        recording = false;
                        ratatui::restore();
                        let result = crate::configure::run(
                            service.clone(),
                            runtime.clone(),
                            downloads.clone(),
                        );
                        terminal = ratatui::try_init()?;
                        if let Err(error) = result {
                            log(&mut logs, Color::Red, format!("Configuration: {error:#}"));
                        }
                        settings = service.settings();
                        pending = Some(start(&service, &runtime, &downloads, settings.clone()));
                    }
                    Job::Setup => {
                        recording = false;
                        setup = true;
                        selection.select(Some(0));
                    }
                    Job::Exit => break,
                },
                Ok(Err(error)) => log(
                    &mut logs,
                    Color::Red,
                    format!("{error:#}. Use Config or Resume recording to retry."),
                ),
                Err(error) => log(&mut logs, Color::Red, error.to_string()),
            }
        }
        let actions = if setup {
            if first_run {
                vec!["Complete setup →", "Explore notes", "Quit"]
            } else {
                vec!["Complete setup →", "Explore notes", "← Back to recording"]
            }
        } else {
            vec![
                if recording {
                    "Pause recording"
                } else {
                    "Resume recording"
                },
                "Config",
                "Explore notes",
                "Speakers",
                "Setup",
                "Quit",
            ]
        };
        let action_count = actions.len().saturating_sub(1);
        let translation_warning = models::translation_warning(&settings);
        terminal.draw(|frame| {
            let rows = Layout::vertical([Constraint::Length(7), Constraint::Min(8), Constraint::Length(if translation_warning.is_some() { 7 } else { 4 }), Constraint::Length(1)]).split(frame.area());
            let state = if downloads.any_active() {
                "Downloading models"
            } else if setup {
                "First-run setup"
            } else if pending.is_some() {
                "Finishing processing"
            } else if recording {
                "● Listening"
            } else {
                "Paused"
            };
            frame.render_widget(Block::default().style(Style::default().bg(tui::BACKGROUND)), frame.area());
            let header = Layout::horizontal([Constraint::Min(30), Constraint::Length(if rows[0].width >= 110 { 58 } else { rows[0].width / 2 })]).split(rows[0]);
            usage.render(frame, header[1]);
            frame.render_widget(Paragraph::new(Line::from(vec![Span::styled(" ◆ contextd ", Style::default().fg(tui::ACCENT).add_modifier(Modifier::BOLD)), Span::styled(format!("  {state}  ·  {}  ·  input {:.4}", if processing { "Processing speech" } else { "Waiting for speech" }, level), Style::default().fg(if recording { Color::Green } else { Color::Yellow }))])).block(tui::panel(" Local • private • on-device ")), header[0]);
            let columns = Layout::horizontal([Constraint::Length(26), Constraint::Min(20)]).split(rows[1]);
            frame.render_stateful_widget(List::new(actions.into_iter().map(ListItem::new).collect::<Vec<_>>()).highlight_symbol("› ").highlight_style(Style::default().fg(tui::ACCENT).bg(tui::SELECTED).add_modifier(Modifier::BOLD)).block(tui::panel(" Navigation ")), columns[0], &mut selection);
            if setup {
                frame.render_widget(Paragraph::new(format!("Welcome to contextd\n\nChoose your settings before recording starts.\n\nSpeech model: {}\n  h / l or ← / →: choose speech model · ? for help\n\nCleanup: {} · {}\n  c: enable / disable · f: choose cleanup model\n\nKeep audio clips: {}\n  a: toggle WAV retention\n\nMicrophone: {}\n  m: cycle available microphones\n\nEnter: save setup, download missing models, and begin\n\nModel downloads show live progress below.{}", settings.speech_model, if settings.filter_enabled { "enabled" } else { "disabled" }, models::catalog(&service.paths).iter().find(|m| m.id == models::canonical_cleanup_id(&settings.filter_model)).map(|m| format!("{} (~{} MB)", m.name, m.size_mb)).unwrap_or_else(|| settings.filter_model.clone()), settings.retain_audio, settings.microphone.as_deref().unwrap_or("System default"), if first_run { "\n\nBackspace / Esc aborts setup and exits; nothing is saved." } else { "\n\nBackspace / Esc goes back to recording; unsaved changes are discarded." })).wrap(Wrap { trim: false }).block(Block::bordered().title(" Setup ").border_style(Style::default().fg(Color::Cyan))), columns[1]);
            } else {
                let paragraph = Paragraph::new(logs.iter().cloned().collect::<Vec<_>>()).wrap(Wrap { trim: false });
                let max = paragraph.line_count(columns[1].width.saturating_sub(2).max(1)).saturating_sub(usize::from(columns[1].height.saturating_sub(2))).min(u16::MAX as usize) as u16;
                scroll = scroll.min(max);
                frame.render_widget(paragraph.scroll((max.saturating_sub(scroll), 0)).block(tui::panel(" Live processing log ")), columns[1]);
            }
            if let Some((id, progress)) = downloads.snapshot().first() {
                let ratio = progress.ratio();
                let queued = downloads.queued(id);
                frame.render_widget(Gauge::default().ratio(ratio).gauge_style(Style::default().fg(Color::Cyan))
                    .label(progress.label(id, queued))
                    .block(tui::panel(format!(" {} {id} ", progress.title()))), rows[2]);
            } else {
                let mut processing_info = vec![Line::from(if setup { format!("Language: {} · v: auto / English / Hindi / Hindi + English", settings.language) } else { format!("Models: {} · language {} · cleanup {} · saved text {}", settings.speech_model, settings.language, if settings.filter_enabled { settings.filter_model.as_str() } else { "disabled" }, models::output_language_name(&settings.output_language)) })];
                processing_info.push(Line::from(format!("Audio: {} microphone input(s) · device output {}", settings.selected_microphones().len(), if settings.record_device_audio { "on" } else { "off" })));
                if let Some(message) = &translation_warning {
                    processing_info.push(Line::styled(message.clone(), Style::default().fg(Color::Yellow)));
                }
                 frame.render_widget(Paragraph::new(processing_info).wrap(Wrap { trim: false }).block(tui::panel(" Local processing ")), rows[2]);
            }
            let hints: Vec<Span> = if downloads.any_active() {
                vec![Span::raw("hjkl / arrows: navigate · "), Span::styled("?: help", Style::default().fg(Color::Yellow)), Span::raw(if setup && !first_run { " · Esc / Backspace: cancel and go back" } else { " · q: cancel and exit" })]
            } else {
                vec![Span::raw("hjkl / arrows: navigate · "), Span::styled("?: help", Style::default().fg(Color::Yellow)), Span::raw(" · Enter: open · Backspace: back · q / Esc: stop and exit")]
            };
            frame.render_widget(Paragraph::new(Line::from(hints)).style(Style::default().fg(tui::MUTED)), rows[3]);
            if help {
                tui::help(frame, "Dashboard help", vec![
                    tui::section("Navigation"),
                    tui::keys("j / ↓, k / ↑", "Move the sidebar selection"),
                    tui::keys("g, G", "First and last sidebar item"),
                    tui::keys("h / l, ← / →", "Cycle models during setup"),
                    tui::keys("Enter", "Open the selected item"),
                    tui::keys("Backspace", "Leave Setup and resume recording"),
                    tui::section("Setup"),
                    tui::keys("c", "Enable or disable cleanup"),
                    tui::keys("f", "Cycle cleanup models"),
                    tui::keys("a", "Toggle audio clip retention"),
                     tui::keys("m", "Cycle microphones"),
                     tui::keys("v", "Cycle language; Hindi presets select multilingual models"),
                    tui::section("Live processing log"),
                    tui::keys("PgUp / PgDn", "Scroll back and forward"),
                    tui::keys("Ctrl+u / Ctrl+d", "Scroll by ten lines"),
                    tui::keys("End", "Jump to the newest entries"),
                    tui::section("Resource usage"),
                    tui::keys("CPU / RAM graphs", "Last 60 seconds, sampled once per second; includes child workers"),
                    tui::keys("CPU", "100% is one busy CPU core; multiple cores can exceed 100%"),
                    tui::keys("RAM / peak", "Resident MiB now / highest since dashboard start; shared pages may be counted twice"),
                    tui::section("Other"),
                    tui::keys("q / Esc / Ctrl+C", "Stop recording and exit"),
                ]);
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
        // Esc / Backspace abandon an in-flight download and stay on the dashboard.
        let downloading = pending
            .as_ref()
            .is_some_and(|job| matches!(job.job, Job::Start));
        if downloading && matches!(key.code, KeyCode::Esc | KeyCode::Backspace) {
            if let Some(mut active) = pending.take() {
                active.join(&runtime);
            }
            downloads.cancel_all();
            log(
                &mut logs,
                Color::Yellow,
                "Download cancelled; recording was not started. Resume recording to retry.".into(),
            );
            continue;
        }
        if key.code == KeyCode::Char('q')
            || key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
            || key.code == KeyCode::Esc && !(setup && !first_run)
            || key.code == KeyCode::Backspace && setup && first_run
        {
            if pending.as_ref().is_some_and(|p| matches!(p.job, Job::Exit)) {
                continue;
            }
            pending = Some(stop_all(&service, &runtime, &downloads, pending.take()));
            continue;
        }
        if pending.is_some() {
            // A download or stop job is running: navigation and help stay live, and
            // leaving Setup cancels the in-flight download instead of waiting.
            let selected = selection.selected().unwrap_or(0);
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    selection.select(Some(selected.saturating_sub(1)))
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selection.select(Some((selected + 1).min(action_count)))
                }
                KeyCode::Char('g') => selection.select(Some(0)),
                KeyCode::Char('G') => selection.select(Some(action_count)),
                KeyCode::Esc | KeyCode::Backspace if setup && !first_run => {
                    if let Some(mut active) = pending.take() {
                        active.join(&runtime);
                    }
                    downloads.cancel_all();
                    setup = false;
                    settings = service.settings();
                    selection.select(Some(0));
                    log(
                        &mut logs,
                        Color::Yellow,
                        "Download cancelled; setup left unchanged. Resume recording to retry."
                            .into(),
                    );
                }
                _ => {}
            }
            // Enter released the waiting job above, so it still acts on the sidebar.
            if key.code != KeyCode::Enter {
                continue;
            }
        }
        let selected = selection.selected().unwrap_or(0);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => selection.select(Some(selected.saturating_sub(1))),
            KeyCode::Down | KeyCode::Char('j') => {
                selection.select(Some((selected + 1).min(action_count)))
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') if setup => {
                let catalog: Vec<_> = models::catalog(&service.paths)
                    .into_iter()
                    .filter(|m| m.kind == "speech")
                    .collect();
                let index = catalog
                    .iter()
                    .position(|m| m.id == settings.speech_model)
                    .unwrap_or(0);
                let next = if matches!(key.code, KeyCode::Left | KeyCode::Char('h')) {
                    (index + catalog.len() - 1) % catalog.len()
                } else {
                    (index + 1) % catalog.len()
                };
                settings.speech_model = catalog[next].id.clone();
            }
            KeyCode::Char('c') if setup => settings.filter_enabled = !settings.filter_enabled,
            KeyCode::Char('f') if setup => {
                let catalog: Vec<_> = models::catalog(&service.paths)
                    .into_iter()
                    .filter(|m| m.kind == "filter")
                    .collect();
                let index = catalog
                    .iter()
                    .position(|m| m.id == models::canonical_cleanup_id(&settings.filter_model))
                    .unwrap_or(0);
                settings.filter_model = catalog[(index + 1) % catalog.len()].id.clone();
            }
            KeyCode::Char('a') if setup => settings.retain_audio = !settings.retain_audio,
            KeyCode::Char('v') if setup => settings.cycle_language(),
            KeyCode::Backspace if setup && !first_run => {
                setup = false;
                settings = service.settings();
                selection.select(Some(0));
                log(
                    &mut logs,
                    Color::Gray,
                    "Setup cancelled; recording resumed with saved settings.".into(),
                );
                pending = Some(start(&service, &runtime, &downloads, settings.clone()));
            }
            KeyCode::Char('m') if setup => match contextd_core::audio::microphones() {
                Ok(mics) => {
                    settings.microphone = match settings
                        .microphone
                        .as_ref()
                        .and_then(|id| mics.iter().position(|m| &m.id == id))
                    {
                        Some(index) => mics.get(index + 1).map(|m| m.id.clone()),
                        None => mics.first().map(|m| m.id.clone()),
                    };
                }
                Err(error) => log(&mut logs, Color::Red, error.to_string()),
            },
            KeyCode::PageUp | KeyCode::Char('u')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                scroll = scroll.saturating_add(10)
            }
            KeyCode::PageDown | KeyCode::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                scroll = scroll.saturating_sub(10)
            }
            KeyCode::Char('g') => selection.select(Some(0)),
            KeyCode::Char('G') => selection.select(Some(action_count)),
            KeyCode::End => scroll = 0,
            KeyCode::Enter => {
                if setup && selected == 0 {
                    // Persist setup before downloading so it survives network failures.
                    match runtime.block_on(service.save_settings(settings.clone())) {
                        Ok(()) => {
                            setup = false;
                            selection.select(Some(0));
                            pending = Some(start(&service, &runtime, &downloads, settings.clone()));
                        }
                        Err(error) => log(&mut logs, Color::Red, error.to_string()),
                    }
                } else if setup && selected == 1 {
                    ratatui::restore();
                    let result = crate::explore::run(service.paths.clone());
                    terminal = ratatui::try_init()?;
                    if let Err(error) = result {
                        log(&mut logs, Color::Red, error.to_string());
                    }
                } else if setup && first_run {
                    let service = service.clone();
                    pending = Some(Pending {
                        job: Job::Exit,
                        task: stop_task(&service, &runtime),
                    });
                } else if setup {
                    setup = false;
                    settings = service.settings();
                    selection.select(Some(0));
                    log(
                        &mut logs,
                        Color::Gray,
                        "Setup cancelled; recording resumed with saved settings.".into(),
                    );
                    pending = Some(start(&service, &runtime, &downloads, settings.clone()));
                } else if selected == 2 {
                    ratatui::restore();
                    let result = crate::explore::run(service.paths.clone());
                    terminal = ratatui::try_init()?;
                    if let Err(error) = result {
                        log(&mut logs, Color::Red, error.to_string());
                    }
                } else if selected == 3 {
                    ratatui::restore();
                    let result = crate::speakers::run(service.paths.clone());
                    terminal = ratatui::try_init()?;
                    if let Err(error) = result {
                        log(&mut logs, Color::Red, error.to_string());
                    }
                } else if selected == 4 || selected == 5 {
                    let service = service.clone();
                    pending = Some(Pending {
                        job: if selected == 5 { Job::Exit } else { Job::Setup },
                        task: stop_task(&service, &runtime),
                    });
                } else if selected == 1 || recording {
                    let service = service.clone();
                    pending = Some(Pending {
                        job: if selected == 1 {
                            Job::Config
                        } else {
                            Job::Stop
                        },
                        task: stop_task(&service, &runtime),
                    });
                } else {
                    pending = Some(start(&service, &runtime, &downloads, settings.clone()));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines_report_each_percent() {
        let (message, percent) = download_message("whisper-tiny", 7_770_000, 77_700_000).unwrap();
        assert_eq!(percent, 10);
        assert!(message.contains("10%"), "{message}");
        assert!(message.contains("7.8 / 77.7 MB"), "{message}");
        // Bytes inside the same percent still report, so callers can compare percent.
        let (_, same) = download_message("whisper-tiny", 7_779_000, 77_700_000).unwrap();
        assert_eq!(same, 10);
    }

    #[test]
    fn completed_download_reports_the_installed_size() {
        let (message, percent) = download_message("whisper-tiny", 77_700_000, 77_700_000).unwrap();
        assert_eq!(percent, 100);
        assert_eq!(message, "Downloaded whisper-tiny: 77.7 MB installed");
    }

    #[test]
    fn missing_length_reports_no_progress() {
        assert!(download_message("whisper-tiny", 1_024, 0).is_none());
    }
}
