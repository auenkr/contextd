use anyhow::Result;
use clap::{Parser, Subcommand};
use contextd_core::{
    audio,
    config::{DATA_HOME_ENV, PRODUCT_NAME},
    models, Event, Service,
};
use std::io::{self, Write};
mod explore;

#[derive(Parser)]
#[command(
    name = PRODUCT_NAME,
    version,
    about = "Continuously build local context from speech for you and your agents"
)]
struct Args {
    /// Open the saved notes and audio browser (also available as `explore`).
    #[arg(long)]
    explore: bool,
    /// Use a portable directory for settings, data, models, and logs.
    #[arg(long, global = true)]
    data_dir: Option<std::path::PathBuf>,
    /// Keep processed audio clips as WAV files.
    #[arg(long, global = true, conflicts_with = "no_retain_audio", action = clap::ArgAction::SetTrue)]
    retain_audio: bool,
    /// Do not keep processed audio clips.
    #[arg(long, global = true, action = clap::ArgAction::SetTrue)]
    no_retain_audio: bool,
    /// Show microphone and processing diagnostics.
    #[arg(long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Browse saved notes and play retained audio in the terminal.
    Explore,
    /// Continuously record your selected microphone until Ctrl+C.
    #[command(alias = "record")]
    Start {
        #[arg(long)]
        microphone: Option<String>,
    },
    /// List microphone IDs for config or --microphone.
    Microphones,
    /// Show the resolved config, data, cache, and log directories.
    Paths,
    Models,
    Download {
        id: String,
        #[arg(long)]
        select: bool,
    },
    /// Display settings or apply a JSON settings file.
    Config {
        #[arg(long)]
        file: Option<std::path::PathBuf>,
    },
    Transcribe {
        file: std::path::PathBuf,
    },
    Refine {
        text: String,
    },
    Note {
        text: String,
    },
    Search {
        #[arg(default_value = "")]
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    Export {
        #[arg(long, default_value = "markdown")]
        format: String,
    },
    Delete {
        id: String,
    },
    /// Read-only loopback retrieval API. Use with --record for continuous capture.
    Serve {
        #[arg(long, default_value_t = 3210)]
        port: u16,
        #[arg(long)]
        record: bool,
    },
    Unload,
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(path) = &args.data_dir {
        std::env::set_var(DATA_HOME_ENV, path);
    }
    if args.explore || matches!(args.command, Some(Command::Explore)) {
        if args.explore && args.command.is_some() {
            anyhow::bail!("Use either --explore or the explore command");
        }
        if args.retain_audio || args.no_retain_audio {
            anyhow::bail!("Audio retention flags apply to recording; use config to change them");
        }
        let paths = contextd_core::Paths::discover()?;
        return tokio::task::spawn_blocking(move || explore::run(paths)).await?;
    }
    let service = Service::new()?;
    let first_run = !service.paths.settings_path().exists();
    let retain_audio_override = if args.retain_audio {
        Some(true)
    } else if args.no_retain_audio {
        Some(false)
    } else {
        None
    };
    if let Some(retain_audio) = retain_audio_override {
        let mut settings = service.settings();
        settings.retain_audio = retain_audio;
        service.save_settings(settings).await?;
    }
    let mut events = service.events.subscribe();
    let (log_stop, mut stopped) = tokio::sync::oneshot::channel();
    let log_task = tokio::spawn(async move {
        let mut last_progress = (String::new(), 101u64);
        loop {
            tokio::select! {
                _ = &mut stopped => {
                    // Short commands and shutdown must print their final saved entries
                    // before the runtime drops this task.
                    loop {
                        match events.try_recv() {
                            Ok(event) => print_event(event, &mut last_progress, args.verbose),
                            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(count)) =>
                                eprintln!("Warning: skipped {count} console events; saved completions remain in the processing log."),
                            Err(_) => break,
                        }
                    }
                    break;
                }
                event = events.recv() => match event {
                    Ok(event) => print_event(event, &mut last_progress, args.verbose),
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) =>
                        eprintln!("Warning: skipped {count} console events; saved completions remain in the processing log."),
                }
            }
        }
    });
    let result = async {
        match args.command {
            command @ (None | Some(Command::Start { .. })) => {
                if let Some(Command::Start {
                    microphone: Some(microphone),
                }) = command
                {
                    let mut settings = service.settings();
                    settings.microphone = Some(microphone);
                    service.save_settings(settings).await?;
                }
                let settings = if first_run {
                    startup_guide(&service, retain_audio_override).await?
                } else {
                    service.settings()
                };
                ensure_selected_models(&service, &settings).await?;
                service.start().await?;
                eprintln!(
                    "{} data: {} (processed audio clips: {})",
                    contextd_core::config::PRODUCT_NAME,
                    service.paths.root.display(),
                    if settings.retain_audio {
                        "saved"
                    } else {
                        "not saved"
                    }
                );
                eprintln!("Listening. Speech is processed automatically. Ctrl+C to stop.");
                shutdown_signal().await?;
                service.stop().await?;
            }
            Some(command) => match command {
                Command::Explore => unreachable!("explore is handled before service startup"),
                Command::Microphones => {
                    println!("{}", serde_json::to_string_pretty(&audio::microphones()?)?)
                }
                Command::Paths => println!("{}", serde_json::to_string_pretty(&service.paths)?),
                Command::Models => {
                    let models = models::list(&service.paths).await?;
                    println!("{}", serde_json::to_string_pretty(&models)?);
                }
                Command::Download { id, select } => {
                    models::download(&service.paths, &id, &service.events).await?;
                    if select {
                        let mut settings = service.settings();
                        if models::catalog(&service.paths)
                            .iter()
                            .any(|m| m.id == id && m.kind == "speech")
                        {
                            settings.speech_model = id;
                        } else {
                            settings.filter_model = id;
                        }
                        service.save_settings(settings).await?;
                    }
                }
                Command::Config { file } => {
                    if let Some(path) = file {
                        service
                            .save_settings(serde_json::from_slice(&std::fs::read(path)?)?)
                            .await?;
                    }
                    println!("{}", serde_json::to_string_pretty(&service.settings())?);
                }
                Command::Start { .. } => unreachable!("start is handled above"),
                Command::Transcribe { file } => {
                    if let Some(entry) = service
                        .process(audio::read_wav(&file)?, service.settings())
                        .await?
                    {
                        println!("{}", serde_json::to_string_pretty(&entry)?);
                    }
                }
                Command::Refine { text } => {
                    println!("{}", service.refine(&text, service.settings()).await?)
                }
                Command::Note { text } => {
                    println!("{}", serde_json::to_string_pretty(&service.note(&text)?)?)
                }
                Command::Search { query, limit } => println!(
                    "{}",
                    serde_json::to_string_pretty(&service.history(&query, limit)?)?
                ),
                Command::Export { format } => {
                    let entries = service.export()?;
                    match format.as_str() {
                        "json" => println!("{}", serde_json::to_string_pretty(&entries)?),
                        "markdown" => {
                            for entry in entries.iter().rev() {
                                println!(
                                    "## {}\n\n{}\n\n<!-- memory-id: {} -->\n",
                                    entry.created_at, entry.refined, entry.id
                                );
                            }
                        }
                        _ => anyhow::bail!("Format must be json or markdown"),
                    }
                }
                Command::Delete { id } => service.delete(&id)?,
                Command::Unload => service.unload().await?,
                Command::Serve { port, record } => {
                    if record {
                        service.start().await?;
                    }
                    eprintln!("Context API: http://127.0.0.1:{port}/v1/memories?q=your+query");
                    tokio::select! {
                        result = contextd_core::context::serve(service.clone(), port) => {
                            service.stop().await?;
                            result?;
                        },
                        result = shutdown_signal() => { result?; }
                    }
                    service.stop().await?;
                }
            },
        }
        Ok(())
    }
    .await;
    let _ = log_stop.send(());
    let _ = log_task.await;
    result
}

fn print_event(event: Event, last_progress: &mut (String, u64), verbose: bool) {
    if !verbose && matches!(event, Event::Activity { .. } | Event::Unloaded) {
        return;
    }
    if matches!(
        event,
        Event::Processed { .. } | Event::Activity { .. } | Event::Error { .. } | Event::Unloaded
    ) && !last_progress.0.is_empty()
        && last_progress.1 < 100
    {
        eprintln!();
        *last_progress = (String::new(), 101);
    }
    match event {
        Event::Activity { log } => eprintln!("[{}] {}", log.timestamp, log.message),
        Event::Processed { log } => {
            let speech_time = chrono::DateTime::parse_from_rfc3339(&log.started_at)
                .map(|time| {
                    time.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M:%S %:z")
                        .to_string()
                })
                .unwrap_or_else(|_| log.started_at.clone());
            eprintln!("\n[{speech_time}]");
            if verbose {
                eprintln!("  Processed: {} ({})", log.processed_at, log.id);
            }
            // JSON quoting keeps multiline text on one log line without losing it.
            eprintln!(
                "  Original text: {}",
                serde_json::to_string(&log.raw_text).unwrap()
            );
            eprintln!(
                "  Saved text: {}",
                serde_json::to_string(&log.saved_text).unwrap()
            );
            eprintln!("  Note saved: {}", log.note_path);
            if let Some(path) = log.audio_path {
                eprintln!("  Audio saved: {path}");
            }
            if verbose {
                eprintln!("  History saved: {}", log.history_path);
            }
            if let Some(error) = log.filter_error {
                eprintln!("  Cleanup failed; original text saved: {error}");
            }
        }
        Event::Error { message } => eprintln!("Error: {message}"),
        Event::Unloaded => eprintln!("Models unloaded after idle or explicit request."),
        Event::Download {
            id,
            completed,
            total,
        } if total > 0 => {
            let percent = completed.saturating_mul(100) / total;
            if *last_progress != (id.clone(), percent) {
                if last_progress.0 != id && !last_progress.0.is_empty() {
                    eprintln!();
                }
                eprint!("\r{id}: {percent}%");
                let _ = io::stderr().flush();
                if percent >= 100 {
                    eprintln!();
                }
                *last_progress = (id, percent);
            }
        }
        _ => {}
    }
}

async fn startup_guide(
    service: &Service,
    retain_audio_override: Option<bool>,
) -> Result<contextd_core::Settings> {
    let mut settings = service.settings();
    eprintln!("\nWelcome to {PRODUCT_NAME}\n\nChoose a speech model:");
    eprintln!("  1) Parakeet Unified EN 0.6B (recommended, English, fast, ~731 MB)");
    eprintln!("  2) Whisper Base (multilingual, balanced, ~142 MB)");
    let speech_choice = prompt("Speech model [1]: ")?;
    let speech_id = if speech_choice.trim() == "2" {
        "whisper-base"
    } else {
        "parakeet-unified-en-0.6b"
    };
    settings.speech_model = speech_id.to_owned();

    let cleanup_choice =
        prompt("Enable speech cleanup to remove filler words and repetitions? [Y/n]: ")?;
    settings.filter_enabled = !matches!(
        cleanup_choice.trim().to_ascii_lowercase().as_str(),
        "n" | "no"
    );
    if retain_audio_override.is_none() {
        let retain_choice = prompt("Keep processed audio clips as WAV files? [y/N]: ")?;
        settings.retain_audio = matches!(
            retain_choice.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        );
    }
    if settings.filter_enabled {
        eprintln!("\nCleanup model:");
        eprintln!("  S1-mini (transcript cleanup, English, ~484 MB; runs locally in this app)");
        let _ = prompt("Press Enter to use S1-mini: ")?;
        let id = models::DEFAULT_FILTER_MODEL;
        settings.filter_model = id.to_owned();
    }

    service.save_settings(settings.clone()).await?;
    Ok(settings)
}

async fn ensure_selected_models(
    service: &Service,
    settings: &contextd_core::Settings,
) -> Result<()> {
    eprintln!("\nDownloading selected models when needed.");
    models::download(&service.paths, &settings.speech_model, &service.events).await?;
    if settings.filter_enabled {
        models::download(&service.paths, &settings.filter_model, &service.events).await?;
    }
    Ok(())
}

fn prompt(label: &str) -> Result<String> {
    eprint!("{label}");
    io::stderr().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input)
}

async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    Ok(())
}
