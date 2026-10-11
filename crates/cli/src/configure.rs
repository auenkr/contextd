use crate::tui::{panel, ACCENT, BACKGROUND, MUTED, SELECTED};
use crate::{downloads::Downloads, tui};
use anyhow::{bail, Result};
use contextd_core::{
    audio, config::CleanupBackend, device::DeviceProfile, models, Service, Settings,
};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Gauge, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};
use std::{
    io::{self, IsTerminal},
    sync::Arc,
    time::Duration,
};

struct RestoreTerminal;
struct Editing {
    key: String,
    value: String,
    choices: Vec<Choice>,
    selection: ListState,
    custom: bool,
}

struct Choice {
    label: String,
    description: String,
    value: Option<String>,
}

fn setting_info(key: &str) -> (&str, &str) {
    match key {
        "preferred_language" => ("Preferred language", "Guide automatic recognition and model recommendations. Choosing a preference resets the explicit language override to auto."),
        "language" => ("Language override", "Auto follows your preferred language. An explicit language overrides it. Hindi + English starts with automatic detection, then retries Hindi when needed."),
        "cleanup_backend" => ("Cleanup engine", "Auto chooses the engine from the model. GGUF runs locally through llama.cpp. MLX requires Apple Silicon macOS. Changing this can select a compatible cleanup model."),
        "short_transcript_policy" => ("Short transcripts", "Decide what to do with very short speech: preserve it, clean it, or discard it."),
        "filter_enabled" => ("Transcript cleanup", "Remove fillers and repetitions after recognition. Original transcripts are preserved; cleanup needs a downloaded model."),
        "retain_audio" => ("Keep audio recordings", "Off saves text without retaining completed audio recordings. On keeps recordings alongside transcripts and uses more disk space."),
        "silence_ms" => ("Pause before saving", "How much silence ends a speech segment. Allowed: 300–5000 ms. Shorter pauses save sooner; longer pauses keep thoughts together."),
        "speech_threshold" => ("Minimum voice volume", "Minimum RMS energy before speech can start. Allowed: 0.001–0.2. Lower values help quiet voices; higher values reject low-level noise."),
        "vad_threshold" => ("Speech confidence", "Silero speech-confidence gate. Allowed: 0.1–0.95. Higher values reject more noise but may miss quiet speech."),
        "idle_unload_seconds" => ("Release idle models", "Unload models after inactivity to free memory. Allowed: 0–86400 seconds; 0 disables idle unloading."),
        "data_directory" => ("Notes & audio folder", "Use an absolute directory path. Applies after restart; existing archives stay where they are. Default clears the override."),
        "cache_directory" => ("Model cache folder", "Use an absolute directory path. Applies after restart; downloaded weights are not moved. Default clears the override."),
        "logs_directory" => ("Logs folder", "Use an absolute directory path. Applies after restart. Default clears the override."),
        "cleanup_source" => ("Custom cleanup source", "GGUF: absolute model path or HTTPS URL. MLX: repository ID or absolute model folder. Default restores a catalog model."),
        "mlx_python" => ("MLX Python runtime", "Default lets uv manage a Python environment. Custom accepts a Python executable with mlx-lm installed."),
        "speech_model" => ("Speech recognition model", "Browse models with descriptions, download sizes, cache status, and device recommendations in the Speech section."),
        "filter_model" => ("Cleanup model", "Browse local cleanup models and compare their descriptions in the Cleanup section."),
        "microphone" => ("Primary microphone", "Choose inputs in the Microphones section. A simultaneous input list takes precedence over this legacy single-input setting."),
        "microphones" => ("Simultaneous microphones", "Toggle one or more inputs in the Microphones section. Audio stays separate; saved transcripts identify each source."),
        _ => (key, "Choose a value below, or enter a custom value."),
    }
}

fn setting_choices(key: &str) -> Vec<Choice> {
    let presets: Vec<(&str, &str, &str)> = match key {
        "language" | "preferred_language" => vec![
            ("auto", "Automatic", "Detect language / follow preference"),
            ("en", "English", "English speech"),
            ("hi", "Hindi", "Hindi in Devanagari"),
            ("hi-en", "Hindi + English", "Mixed-language speech"),
        ],
        "cleanup_backend" => vec![
            ("auto", "Automatic", "Use the selected model's engine"),
            ("gguf", "GGUF", "Portable local llama.cpp engine"),
            ("mlx", "MLX", "Apple Silicon only"),
        ],
        "short_transcript_policy" => vec![
            ("keep", "Keep", "Preserve short speech unchanged"),
            (
                "cleanup",
                "Clean up",
                "Remove filler while preserving meaning",
            ),
            ("skip_all", "Skip all", "Discard all short transcripts"),
        ],
        "filter_enabled" | "retain_audio" => {
            vec![("false", "Off", "Disabled"), ("true", "On", "Enabled")]
        }
        "silence_ms" => vec![
            ("600", "Quick · 600 ms", "Save soon after a pause"),
            (
                "1400",
                "Balanced · 1400 ms",
                "Default; natural sentence pauses",
            ),
            ("2500", "Relaxed · 2500 ms", "Keep longer thoughts together"),
        ],
        "speech_threshold" => vec![
            (
                "0.006",
                "Quiet voice · 0.006",
                "More sensitive to low-volume speech",
            ),
            ("0.012", "Balanced · 0.012", "Default minimum energy"),
            ("0.03", "Louder voice · 0.03", "Ignore more low-level sound"),
        ],
        "vad_threshold" => vec![
            ("0.4", "Sensitive · 0.4", "Accept lower-confidence speech"),
            ("0.6", "Balanced · 0.6", "Default speech confidence"),
            ("0.8", "Strict · 0.8", "Stronger noise rejection"),
        ],
        "idle_unload_seconds" => vec![
            ("0", "Keep loaded", "Disable idle unloading"),
            ("60", "After 1 minute", "Release memory sooner"),
            ("120", "After 2 minutes", "Default idle timeout"),
            ("300", "After 5 minutes", "Reduce model reloads"),
        ],
        _ => vec![("null", "Use default", "Clear the custom override")],
    };
    let mut choices: Vec<_> = presets
        .into_iter()
        .map(|(value, label, description)| Choice {
            label: label.into(),
            description: description.into(),
            value: Some(value.into()),
        })
        .collect();
    if !matches!(
        key,
        "cleanup_backend" | "short_transcript_policy" | "filter_enabled" | "retain_audio"
    ) {
        choices.push(Choice {
            label: "Custom value…".into(),
            description: "Enter your own value; Esc returns without saving".into(),
            value: None,
        });
    }
    choices
}

impl Editing {
    fn new(key: &str, value: &serde_json::Value) -> Self {
        let mut choices = setting_choices(key);
        let value = value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string());
        let selected = choices.iter().position(|choice| {
            choice.value.as_deref().is_some_and(|preset| {
                preset == value
                    || preset
                        .parse::<f64>()
                        .ok()
                        .zip(value.parse::<f64>().ok())
                        .is_some_and(|(preset, current)| (preset - current).abs() < 0.0000001)
            })
        });
        let selected = selected.unwrap_or_else(|| {
            choices.insert(
                0,
                Choice {
                    label: format!("Current · {value}"),
                    description: "Keep your existing value".into(),
                    value: Some(value.clone()),
                },
            );
            0
        });
        Self {
            key: key.into(),
            value,
            choices,
            selection: ListState::default().with_selected(Some(selected)),
            custom: false,
        }
    }
}

fn apply_edit(
    mut settings: Settings,
    key: &str,
    value: &str,
    device: &DeviceProfile,
) -> Result<Settings> {
    let mut document = serde_json::to_value(&settings)?;
    let old = &document[key];
    let optional = matches!(
        key,
        "microphone"
            | "cleanup_source"
            | "mlx_python"
            | "data_directory"
            | "cache_directory"
            | "logs_directory"
    );
    document[key] = if value.trim() == "null" || value.is_empty() && (old.is_null() || optional) {
        serde_json::Value::Null
    } else if old.is_number() || old.is_array() || old.is_boolean() {
        serde_json::from_str(value)?
    } else {
        serde_json::Value::String(value.into())
    };
    settings = serde_json::from_value(document)?;
    match key {
        "preferred_language" | "language" => {
            if key == "preferred_language" {
                settings.language = "auto".into();
            }
            if settings.speech_model.starts_with("parakeet-")
                && settings.speech_language() != "en"
                && settings.speech_language() != "auto"
            {
                settings.speech_model = device.recommend(&settings).speech_model;
            }
        }
        "cleanup_backend" if settings.cleanup_source.is_none() => {
            if settings.cleanup_backend == CleanupBackend::Mlx {
                settings.filter_model = "qwen3.5:0.8b-mlx".into();
            } else if settings.filter_model.ends_with("-mlx") {
                settings.filter_model = "qwen3.5:0.8b".into();
            }
        }
        "cleanup_source" if settings.cleanup_source.is_some() => {
            settings.cleanup_backend = if settings.uses_mlx() {
                CleanupBackend::Mlx
            } else {
                CleanupBackend::Gguf
            };
            settings.filter_model = "custom".into();
        }
        "cleanup_source" if settings.filter_model == "custom" => {
            settings.filter_model = device.recommend(&settings).cleanup_model;
            settings.cleanup_backend = CleanupBackend::Auto;
        }
        "filter_model" => {
            settings.cleanup_source = None;
            settings.cleanup_backend = CleanupBackend::Auto;
        }
        _ => {}
    }
    settings.validate()?;
    Ok(settings)
}
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
    let mut editing: Option<Editing> = None;
    let device = DeviceProfile::detect();
    let mut last_sync = std::time::Instant::now();
    let mut message = "Select a model with Enter; d deletes a cached copy".to_string();
    loop {
        if last_sync.elapsed().as_secs() >= 1 && editing.is_none() {
            if let Err(error) = runtime.block_on(service.refresh_settings()) {
                message = format!("Config file: {error:#}");
            }
            last_sync = std::time::Instant::now();
        }
        let settings = service.settings();
        let recommendation = device.recommend(&settings);
        let document = serde_json::to_value(&settings)?;
        let fields: Vec<_> = document
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let mut microphones = audio::microphones().unwrap_or_default();
        for id in settings.selected_microphones().into_iter().flatten() {
            if !microphones.iter().any(|mic| mic.id == id) {
                microphones.push(audio::Microphone {
                    id: id.clone(),
                    name: format!("Unavailable: {id}"),
                    is_default: false,
                });
            }
        }
        let catalog: Vec<_> = models::catalog(&service.paths)
            .into_iter()
            .filter(|model| model.kind == if tab == 0 { "speech" } else { "filter" })
            .collect();
        let count = match tab {
            2 => fields.len(),
            3 => microphones.len() + 1,
            _ => catalog.len(),
        };
        let selected = selection.selected().unwrap_or(0).min(count - 1);
        selection.select(Some(selected));
        let active = downloads.snapshot();
        terminal.draw(|frame| {
            let rows = Layout::vertical([
                Constraint::Length(1), Constraint::Length(3), Constraint::Min(5),
                  Constraint::Length(4), Constraint::Length(if active.is_empty() { 0 } else { 6 }), Constraint::Length(3), Constraint::Length(2),
            ]).split(frame.area());
             frame.render_widget(Block::default().style(Style::default().bg(BACKGROUND)), frame.area());
             frame.render_widget(Paragraph::new(Line::from(vec![
                 Span::styled(" ◆ contextd ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                 Span::styled(" / Configuration   ", Style::default().fg(Color::White)),
                 Span::styled(service.paths.settings_path().display().to_string(), Style::default().fg(MUTED)),
             ])), rows[0]);
            frame.render_widget(Tabs::new(["1 Speech to text", "2 Cleanup / processing", "3 All settings", "4 Microphones"])
                 .select(tab).style(Style::default().fg(MUTED)).highlight_style(Style::default().fg(ACCENT).bg(SELECTED).add_modifier(Modifier::BOLD))
                 .divider(" • ").block(panel(" Tab / h / l: switch ")), rows[1]);
            let columns = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).split(rows[2]);
            if tab < 2 {
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
                let recommended = model.id == recommendation.speech_model || model.id == recommendation.cleanup_model;
                 ListItem::new(vec![Line::from(vec![
                     Span::styled(if active { "● " } else { "○ " }, Style::default().fg(if active { ACCENT } else { MUTED })),
                     Span::styled(model.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
                 ]), Line::from(vec![
                     Span::styled(format!("  [{state}] · {} MB", model.size_mb), Style::default().fg(if model.installed { Color::Green } else { MUTED })),
                     Span::styled(if recommended { " ★ recommended" } else { "" }, Style::default().fg(Color::Yellow)),
                 ])])
            }).collect();
            frame.render_stateful_widget(List::new(items).highlight_symbol("› ")
                 .highlight_style(Style::default().bg(SELECTED).fg(Color::White))
                 .block(panel(" Models · j / k ")), columns[0], &mut selection);
            let model = &catalog[selected];
             frame.render_widget(Paragraph::new(format!("{}\n\n{}\n\nApproximate download: {} MB\nCache: {}\n\nDevice: {}\n\nEnter: select/download · r: recommended models\nd: delete the cached copy", model.name, model.description, model.size_mb,
                 service.paths.models_dir().display(), recommendation.reason))
                  .wrap(Wrap { trim: false }).block(panel(" Model details ")), columns[1]);
            } else if tab == 2 {
                 let items: Vec<_> = fields.iter().map(|(key,value)| ListItem::new(Line::from(vec![
                     Span::styled(format!("{}  ", setting_info(key).0), Style::default().fg(Color::White)),
                     Span::styled(value.to_string(), Style::default().fg(if value == &serde_json::Value::Bool(true) { Color::Green } else { ACCENT })),
                 ]))).collect();
                 frame.render_stateful_widget(List::new(items).highlight_symbol("› ").highlight_style(Style::default().bg(SELECTED).add_modifier(Modifier::BOLD)).block(panel(" Every config value · Enter: choose ")), columns[0], &mut selection);
                let (key,value) = &fields[selected];
                 let (label, description) = setting_info(key);
                 let choices = setting_choices(key);
                 let mut details = vec![
                     Line::styled(label.to_string(), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                     Line::styled(key.to_string(), Style::default().fg(MUTED)),
                     Line::from(""), Line::from(description.to_string()), Line::from(""),
                     Line::from(vec![Span::raw("Current: "), Span::styled(value.to_string(), Style::default().fg(Color::Yellow))]),
                     Line::from(""), tui::section("Possible values"),
                 ];
                 if matches!(key.as_str(), "speech_model" | "filter_model" | "microphone" | "microphones") {
                     details.push(Line::from("Enter opens the interactive model / input browser."));
                 } else {
                     details.extend(choices.iter().map(|choice| Line::from(vec![Span::styled(format!("• {}  ", choice.label), Style::default().fg(ACCENT)), Span::raw(choice.description.clone())])));
                 }
                 details.extend([Line::from(""), Line::styled("Enter: choose a value  ·  Esc: cancel", Style::default().fg(Color::Yellow)), Line::from(""), Line::styled("Saved immediately • config file syncs every second", Style::default().fg(MUTED))]);
                 frame.render_widget(Paragraph::new(details).wrap(Wrap { trim:false }).block(panel(" Setting guide ")), columns[1]);
            } else {
                let selected_inputs = settings.selected_microphones();
                let mut items = vec![ListItem::new(format!("[{}] System default (when no explicit input is selected)", if selected_inputs == vec![None] { "x" } else { " " }))];
                items.extend(microphones.iter().map(|mic| ListItem::new(format!("[{}] {} · {}", if selected_inputs.contains(&Some(mic.id.clone())) { "x" } else { " " }, mic.name, mic.id))));
                 frame.render_stateful_widget(List::new(items).highlight_symbol("› ").highlight_style(Style::default().bg(SELECTED).fg(ACCENT)).block(panel(" Inputs · Enter / Space: toggle ")), columns[0], &mut selection);
                 frame.render_widget(Paragraph::new(format!("Select several inputs to record them simultaneously.\n\nEach input has its own resampler and speech detector. Audio is not interleaved or mixed; saved transcripts identify the source microphone.\n\nSelecting System default clears explicit selections. Disconnected selected inputs remain visible.\n\nSelected IDs: {:?}", settings.selected_microphones())).wrap(Wrap {trim:false}).block(panel(" Microphone recording ")), columns[1]);
            }
             frame.render_widget(Paragraph::new(format!("Cleanup: {} (c: toggle) · Retain audio: {} (a: toggle)\nLanguage: {} (v: auto → English → Hindi → Hindi + English)",
                 if settings.filter_enabled { "enabled" } else { "disabled" }, settings.retain_audio, settings.language))
                 .style(Style::default().fg(ACCENT)).block(panel(" Quick controls ")), rows[3]);
            let progress = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).split(rows[4]);
            if let Some((id, first)) = active.first() {
                let ratio = if first.total == 0 { 0.0 } else { (first.completed as f64 / first.total as f64).min(1.0) };
                let queued = downloads.queued(id);
                frame.render_widget(Gauge::default().ratio(ratio).gauge_style(Style::default().fg(Color::Cyan))
                    .label(if queued { format!("{id}: waiting for a free download slot") } else {
                        format!("{id}: {:.0}% · {:.1} / {:.1} MB · q: cancel and go back", ratio * 100.0, first.completed as f64 / 1e6, first.total as f64 / 1e6)
                    })
                     .block(panel(if active.len() > 1 { format!(" Downloading ({} in queue) ", active.len()) } else { " Downloading ".into() })), progress[0]);
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
                 .style(Style::default().fg(MUTED)),
                progress[1],
            );
            frame.render_widget(
                Paragraph::new(message.clone()).style(Style::default().fg(if message.contains("Failed") || message.contains("Invalid") || message.starts_with("Config file:") { Color::LightRed } else { Color::Green })).wrap(Wrap { trim: false }).block(panel(" Status ")),
                rows[5],
            );
            frame.render_widget(
                 Paragraph::new("1–4 / Tab: sections · j/k: navigate · Enter: edit/select · r: recommendations · c: cleanup · a: audio · v: language · ?: help · q: back")
                     .style(Style::default().fg(MUTED)),
                rows[6],
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
                         tui::keys("1 / 2 / 3 / 4", "Speech, cleanup, all settings, microphones"),
                         tui::keys("r", "Apply device/language recommendations and download"),
                         tui::keys("Enter / Space", "Edit settings / select multiple microphone inputs"),
                        tui::section("Actions"),
                        tui::keys("Enter", "Select and download in the background"),
                        tui::keys("d", "Delete a cached model from disk"),
                        tui::keys("c", "Enable or disable cleanup"),
                         tui::keys("a", "Toggle audio clip retention"),
                         tui::keys("v", "Cycle language; Hindi presets select multilingual models"),
                        tui::section("Downloads"),
                        tui::keys("q", "Cancel the download, return to the dashboard"),
                        tui::keys("progress", "Continues on the dashboard while it runs"),
                        tui::section("Other"),
                        tui::keys("? / Esc", "Close this help"),
                    ],
                );
             }
              if let Some(editor) = &mut editing {
                  let popup = tui::centered(frame.area(), (86, 24));
                  frame.render_widget(Clear, popup);
                  let border = panel(format!(" {} · {} ", if editor.custom { "Custom value" } else { "Choose a value" }, setting_info(&editor.key).0)).border_style(Style::default().fg(ACCENT));
                  let inner = border.inner(popup);
                  frame.render_widget(border, popup);
                  let parts = Layout::vertical([Constraint::Length(4), Constraint::Min(2), Constraint::Length(4)]).split(inner);
                  frame.render_widget(Paragraph::new(setting_info(&editor.key).1).wrap(Wrap { trim: false }).style(Style::default().fg(MUTED)), parts[0]);
                  if editor.custom {
                      frame.render_widget(Paragraph::new(vec![Line::styled(editor.key.clone(), Style::default().fg(MUTED)), Line::from(""), Line::styled(format!("{}▏", editor.value), Style::default().fg(ACCENT))]).wrap(Wrap { trim:false }), parts[1]);
                  } else {
                      let items: Vec<_> = editor.choices.iter().map(|choice| ListItem::new(vec![Line::styled(format!("  {}", choice.label), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)), Line::styled(format!("  {}", choice.description), Style::default().fg(MUTED))])).collect();
                      frame.render_stateful_widget(List::new(items).highlight_symbol("› ").highlight_style(Style::default().bg(SELECTED)), parts[1], &mut editor.selection);
                  }
                  frame.render_widget(Paragraph::new(vec![Line::styled(message.clone(), Style::default().fg(if message.starts_with("Invalid") { Color::LightRed } else { MUTED })), Line::from(""), Line::styled(if editor.custom { "Enter: save • Esc: choices • Ctrl+u: clear" } else { "↑/↓ or j/k: browse • Enter: apply • Esc: cancel" }, Style::default().fg(Color::Yellow))]).wrap(Wrap {trim:false}), parts[2]);
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
        if let Some(editor) = &mut editing {
            if !editor.custom {
                let selected = editor.selection.selected().unwrap_or(0);
                match key.code {
                    KeyCode::Esc => {
                        editing = None;
                        message = "No changes made.".into();
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        editor.selection.select(Some(selected.saturating_sub(1)))
                    }
                    KeyCode::Down | KeyCode::Char('j') => editor
                        .selection
                        .select(Some((selected + 1).min(editor.choices.len() - 1))),
                    KeyCode::Char('g') => editor.selection.select(Some(0)),
                    KeyCode::Char('G') => editor.selection.select(Some(editor.choices.len() - 1)),
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        if let Some(value) = &editor.choices[selected].value {
                            match service
                                .paths
                                .load()
                                .and_then(|fresh| apply_edit(fresh, &editor.key, value, &device))
                                .and_then(|next| runtime.block_on(service.save_settings(next)))
                            {
                                Ok(()) => {
                                    message = format!(
                                        "✓ {} saved to config file.",
                                        setting_info(&editor.key).0
                                    );
                                    editing = None;
                                }
                                Err(error) => message = format!("Invalid value: {error:#}"),
                            }
                        } else {
                            editor.custom = true;
                            if editor.value == "null" {
                                editor.value.clear();
                            }
                            message =
                                "Enter a custom value; nothing saves until you press Enter.".into();
                        }
                    }
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Esc => {
                    editor.custom = false;
                    message = "Choose a value, or Esc to cancel.".into();
                }
                KeyCode::Backspace => {
                    editor.value.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    editor.value.clear()
                }
                KeyCode::Char(character) => editor.value.push(character),
                KeyCode::Enter => {
                    match service
                        .paths
                        .load()
                        .and_then(|fresh| apply_edit(fresh, &editor.key, &editor.value, &device))
                        .and_then(|next| runtime.block_on(service.save_settings(next)))
                    {
                        Ok(()) => {
                            message = "Saved to config file.".into();
                            editing = None;
                        }
                        Err(error) => message = format!("Invalid value: {error:#}"),
                    }
                }
                _ => {}
            }
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
            KeyCode::Char(character @ '1'..='4') => {
                tab = character as usize - '1' as usize;
                selection.select(Some(0));
            }
            KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('h')
            | KeyCode::Char('l') => {
                tab = if matches!(
                    key.code,
                    KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab
                ) {
                    (tab + 3) % 4
                } else {
                    (tab + 1) % 4
                };
                selection.select(Some(0));
            }
            KeyCode::Up | KeyCode::Char('k') => selection.select(Some(selected.saturating_sub(1))),
            KeyCode::Down | KeyCode::Char('j') => {
                selection.select(Some((selected + 1).min(count - 1)))
            }
            KeyCode::Char('g') => selection.select(Some(0)),
            KeyCode::Char('G') => selection.select(Some(count - 1)),
            KeyCode::Enter if tab == 2 => {
                let (key, value) = &fields[selected];
                match key.as_str() {
                    "speech_model" => {
                        tab = 0;
                        selection.select(Some(0));
                    }
                    "filter_model" => {
                        tab = 1;
                        selection.select(Some(0));
                    }
                    "microphone" | "microphones" => {
                        tab = 3;
                        selection.select(Some(0));
                    }
                    _ => {
                        editing = Some(Editing::new(key, value));
                        message = "Choose a value; Enter saves, Esc cancels.".into();
                    }
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') if tab == 3 => {
                let mut next = service.paths.load()?;
                let mut inputs: Vec<_> =
                    next.selected_microphones().into_iter().flatten().collect();
                if selected == 0 {
                    inputs.clear();
                } else {
                    let id = &microphones[selected - 1].id;
                    if inputs.contains(id) {
                        inputs.retain(|input| input != id);
                    } else {
                        inputs.push(id.clone());
                    }
                }
                next.microphone = None;
                next.microphones = inputs;
                message = match runtime.block_on(service.save_settings(next)) {
                    Ok(()) => "Microphones saved to config file.".into(),
                    Err(error) => format!("Failed: {error:#}"),
                };
            }
            KeyCode::Enter => {
                let model = catalog[selected].clone();
                let mut next = service.paths.load()?;
                if tab == 0 {
                    next.speech_model = model.id.clone();
                } else {
                    next.filter_model = model.id.clone();
                    next.cleanup_source = if model.id == "custom" {
                        next.cleanup_source
                    } else {
                        None
                    };
                    next.cleanup_backend = if model.id == "custom" {
                        next.cleanup_backend
                    } else {
                        CleanupBackend::Auto
                    };
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
            KeyCode::Char('r') => {
                let mut next = service.paths.load()?;
                next.speech_model = recommendation.speech_model.clone();
                next.filter_model = recommendation.cleanup_model.clone();
                next.cleanup_source = None;
                next.cleanup_backend = CleanupBackend::Auto;
                match runtime.block_on(service.save_settings(next)) {
                    Ok(()) => {
                        for id in [&recommendation.speech_model, &recommendation.cleanup_model] {
                            if !downloads.is_installed(id) {
                                downloads.enqueue(id);
                            }
                        }
                        message =
                            "Recommended models saved; missing weights download in the background."
                                .into();
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
            KeyCode::Char('v') => {
                let mut next = settings;
                next.cycle_language();
                message = match runtime.block_on(service.save_settings(next)) {
                    Ok(()) => {
                        "Language saved; download selected models with Enter if needed.".into()
                    }
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
            KeyCode::Char('d') if tab < 2 => {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pickers_cover_enums_and_select_current_numeric_presets() {
        for (key, expected) in [
            ("cleanup_backend", vec!["auto", "gguf", "mlx"]),
            (
                "short_transcript_policy",
                vec!["keep", "cleanup", "skip_all"],
            ),
            ("retain_audio", vec!["false", "true"]),
        ] {
            let choices = setting_choices(key);
            assert_eq!(
                choices
                    .iter()
                    .map(|choice| choice.value.as_deref().unwrap())
                    .collect::<Vec<_>>(),
                expected
            );
        }
        let document = serde_json::to_value(Settings::default()).unwrap();
        let editor = Editing::new("vad_threshold", &document["vad_threshold"]);
        assert_eq!(editor.selection.selected(), Some(1));
        assert_eq!(editor.choices.len(), 4);
        let editor = Editing::new("silence_ms", &serde_json::json!(1800));
        assert_eq!(editor.choices[0].value.as_deref(), Some("1800"));
        assert!(!editor.custom);
        assert!(editor.choices.last().unwrap().value.is_none());
    }

    #[test]
    fn every_setting_has_a_specific_guide() {
        let document = serde_json::to_value(Settings::default()).unwrap();
        for key in document.as_object().unwrap().keys() {
            assert_ne!(setting_info(key).0, key);
        }
    }

    #[test]
    fn editor_supports_optional_paths_arrays_and_preferred_language() {
        let device = DeviceProfile {
            platform: "linux".into(),
            architecture: "x86_64".into(),
            cpu: "test".into(),
            memory_gb: 16.0,
            available_memory_gb: 8.0,
            apple_silicon: false,
        };
        let settings =
            apply_edit(Settings::default(), "preferred_language", "hi-en", &device).unwrap();
        assert_eq!(settings.speech_model, "qwen3-asr-0.6b");
        assert_eq!(settings.speech_language(), "hi-en");
        let settings = apply_edit(settings, "microphones", r#"["one","two"]"#, &device).unwrap();
        assert_eq!(settings.selected_microphones().len(), 2);
        let settings = apply_edit(settings, "cache_directory", "/absolute/cache", &device).unwrap();
        let settings = apply_edit(settings, "cache_directory", "", &device).unwrap();
        assert!(settings.cache_directory.is_none());
        assert!(apply_edit(settings, "silence_ms", "10", &device).is_err());
    }
}
