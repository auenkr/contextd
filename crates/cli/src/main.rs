use anyhow::Result;
use clap::{Parser, Subcommand};
use contextd_core::{
    audio,
    config::{DATA_HOME_ENV, PRODUCT_NAME},
    models, Event, Service,
};
use std::io::{self, IsTerminal, Write};
mod configure;
mod downloads;
mod editor;
mod explore;
mod home;
mod speakers;
mod tui;

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
    /// Listen to saved voice samples and name speakers.
    Speakers {
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        action: Option<SpeakerCommand>,
    },
    /// List microphone IDs for config or --microphone.
    Microphones,
    /// Show the resolved config, data, cache, and log directories.
    Paths,
    Models,
    /// Show device capabilities and recommended models for the preferred language.
    Recommend,
    /// Remove an unused cached model (selected models are protected).
    RemoveModel {
        id: String,
    },
    /// Download one or more models; multiple downloads run at the same time.
    Download {
        /// Model IDs from the catalog, for example parakeet-unified-en-0.6b whisper-tiny
        #[arg(required = true)]
        ids: Vec<String>,
        #[arg(long)]
        select: bool,
    },
    /// Configure models interactively, display JSON, or apply a settings file.
    Config {
        #[arg(long)]
        file: Option<std::path::PathBuf>,
        #[arg(long)]
        json: bool,
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
    /// Organize completed days into lossless structured Markdown (catch up by default).
    Structure {
        /// Completed local date, YYYY-MM-DD. Omit to process all missed dates.
        #[arg(long)]
        date: Option<chrono::NaiveDate>,
        /// Regenerate even if the source notes have not changed.
        #[arg(long)]
        force: bool,
    },
    Unload,
}
#[derive(Subcommand)]
enum SpeakerCommand {
    /// Rename a voice and synchronize its name in existing notes.
    Name { id: String, name: String },
    /// Retry interrupted name synchronization.
    Sync,
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
        let mut paths = contextd_core::Paths::discover()?;
        paths.apply_storage(&paths.load()?)?;
        return tokio::task::spawn_blocking(move || explore::run(paths)).await?;
    }
    let service = Service::new()?;
    let first_run = !service.paths.settings_path().exists();
    // One download queue serves both screens, so a transfer survives leaving it.
    let downloads =
        crate::downloads::Downloads::new(service.clone(), tokio::runtime::Handle::current());
    if args.command.is_none() {
        service.start_daily_worker();
        let runtime = tokio::runtime::Handle::current();
        let dashboard_service = service.clone();
        let result = tokio::task::spawn_blocking(move || {
            home::run(
                dashboard_service,
                runtime,
                downloads.clone(),
                first_run,
                if args.retain_audio {
                    Some(true)
                } else if args.no_retain_audio {
                    Some(false)
                } else {
                    None
                },
            )
        })
        .await;
        service.stop().await?;
        return result?;
    }
    if matches!(
        args.command,
        Some(Command::Config {
            file: None,
            json: false
        })
    ) && io::stdin().is_terminal()
        && io::stdout().is_terminal()
    {
        let runtime = tokio::runtime::Handle::current();
        return tokio::task::spawn_blocking(move || {
            configure::run(service, runtime, downloads.clone())
        })
        .await?;
    }
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
            None => unreachable!("dashboard is handled above"),
            Some(command) => match command {
                Command::Explore => unreachable!("explore is handled before service startup"),
                Command::Speakers { json, action } => match action {
                    Some(SpeakerCommand::Name { id, name }) => {
                        let paths = service.paths.clone();
                        let settings = service.settings();
                        tokio::task::spawn_blocking(move || contextd_core::speakers::rename(&paths,&settings,&id,&name)).await??;
                        println!("Speaker name saved and notes synchronized");
                    },
                    Some(SpeakerCommand::Sync) => {
                        let paths = service.paths.clone();
                        let settings = service.settings();
                        tokio::task::spawn_blocking(move || contextd_core::speakers::sync_names(&paths,&settings)).await??;
                        println!("Speaker names synchronized");
                    },
                    None if json || !io::stdin().is_terminal() || !io::stdout().is_terminal() => {
                        println!("{}",serde_json::to_string_pretty(&contextd_core::speakers::list(&service.paths)?)?);
                    },
                    None => { let paths = service.paths.clone(); tokio::task::spawn_blocking(move || speakers::run(paths)).await??; }
                },
                Command::Microphones => {
                    println!("{}", serde_json::to_string_pretty(&audio::microphones()?)?)
                }
                Command::Paths => println!("{}", serde_json::to_string_pretty(&service.paths)?),
                Command::Models => {
                    let models = models::list(&service.paths).await?;
                    println!("{}", serde_json::to_string_pretty(&models)?);
                }
                Command::Recommend => {
                    let device = contextd_core::device::DeviceProfile::detect();
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({"recommendation":device.recommend(&service.settings()), "device":device}))?);
                }
                Command::RemoveModel { id } => {
                    let settings = service.settings();
                    let id = models::canonical_cleanup_id(&id);
                    if id == settings.speech_model || id == models::canonical_cleanup_id(&settings.filter_model) || id == settings.daily_structure_model {
                        anyhow::bail!("Select another model before deleting the selected cache");
                    }
                    models::delete(&service.paths, id)?;
                    eprintln!("Removed cached model: {id}");
                }
                Command::Download { ids, select } => {
                    // Queue every model before waiting, so transfers overlap.
                    for id in &ids {
                        downloads.enqueue(id);
                    }
                    let mut selection = None;
                    for id in &ids {
                        if select {
                            selection = Some(id);
                        }
                        downloads.wait(id).await?;
                    }
                    if let Some(id) = selection {
                        let mut settings = service.settings();
                        if models::catalog(&service.paths)
                            .iter()
                            .any(|m| m.id == *id && m.kind == "speech")
                        {
                            settings.speech_model = id.clone();
                        } else {
                            settings.filter_model = id.clone();
                            settings.cleanup_source = None;
                            settings.cleanup_backend = contextd_core::config::CleanupBackend::Auto;
                        }
                        service.save_settings(settings).await?;
                    }
                }
                Command::Config { file, .. } => {
                    if let Some(path) = file {
                        service
                            .save_settings(serde_json::from_slice(&std::fs::read(path)?)?)
                            .await?;
                    }
                    println!("{}", serde_json::to_string_pretty(&service.settings())?);
                }
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
                Command::Structure { date, force } => {
                    for path in service.structure_days(date, force).await? { println!("{}", path.display()); }
                }
                Command::Unload => service.unload().await?,
                Command::Serve { port, record } => {
                    service.start_daily_worker();
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
            if !log.speakers.is_empty() {
                eprintln!("  Speakers: {}", log.speakers.join(", "));
            }
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
