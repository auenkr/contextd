pub mod audio;
pub mod config;
pub mod content;
pub mod context;
pub mod device;
pub mod explore;
pub mod history;
pub mod inference;
pub mod logging;
mod mlx;
pub mod models;
mod queue;

use anyhow::{bail, Result};
pub use config::{Paths, Settings};
pub use history::Transcript;
use inference::{LocalSpeechEngine, SpeechToText};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc, oneshot};

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Status {
        recording: bool,
        processing: bool,
    },
    Level {
        value: f32,
    },
    Transcript {
        entry: Transcript,
    },
    Processed {
        log: logging::ProcessingLog,
    },
    Activity {
        log: logging::ActivityLog,
    },
    Error {
        message: String,
    },
    Download {
        id: String,
        completed: u64,
        total: u64,
    },
    ModelsChanged,
    Unloaded,
}

#[derive(Clone, Serialize)]
pub struct Status {
    pub recording: bool,
    pub processing: bool,
    pub data_directory: String,
}
struct Session {
    stop: oneshot::Sender<()>,
    done: tokio::task::JoinHandle<()>,
}
pub struct Service {
    pub paths: Paths,
    settings: Mutex<Settings>,
    engine: Arc<Mutex<LocalSpeechEngine>>,
    cleanup_engine: Arc<Mutex<inference::LocalCleanupEngine>>,
    pub events: broadcast::Sender<Event>,
    session: tokio::sync::Mutex<Option<Session>>,
    processing: std::sync::atomic::AtomicBool,
    recording: std::sync::atomic::AtomicBool,
}
impl Service {
    pub fn new() -> Result<Arc<Self>> {
        let mut paths = Paths::discover()?;
        let settings = paths.load()?;
        settings.validate()?;
        paths.apply_storage(&settings)?;
        history::History::open(&paths)?;
        let (events, _) = broadcast::channel(256);
        let service = Arc::new(Self {
            engine: Arc::new(Mutex::new(LocalSpeechEngine::new(paths.clone()))),
            cleanup_engine: Arc::new(Mutex::new(inference::LocalCleanupEngine::new())),
            paths,
            settings: Mutex::new(settings),
            events,
            session: tokio::sync::Mutex::new(None),
            processing: std::sync::atomic::AtomicBool::new(false),
            recording: std::sync::atomic::AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&service);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let Some(service) = weak.upgrade() else { break };
                let seconds = service.settings().idle_unload_seconds;
                if let Ok(mut engine) = service.engine.try_lock() {
                    if engine.unload_if_idle(seconds) {
                        let _ = service.events.send(Event::Unloaded);
                    }
                };
                if let Ok(mut engine) = service.cleanup_engine.try_lock() {
                    if engine.unload_if_idle(seconds) {
                        let _ = service.events.send(Event::Unloaded);
                    }
                };
            }
        });
        Ok(service)
    }
    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }
    pub async fn refresh_settings(&self) -> Result<()> {
        let settings = self.paths.load()?;
        settings.validate()?;
        if serde_json::to_value(&settings)? != serde_json::to_value(self.settings())? {
            let session = self.session.lock().await;
            if session.is_some() {
                bail!("Pause capture before reloading settings");
            }
            models::speech_path(&self.paths, &settings.speech_model)?;
            models::cleanup_location(&self.paths, &settings)?;
            self.engine.lock().unwrap().unload();
            let cleanup_engine = self.cleanup_engine.clone();
            tokio::task::spawn_blocking(move || cleanup_engine.lock().unwrap().unload()).await?;
            *self.settings.lock().unwrap() = settings;
        }
        Ok(())
    }
    pub async fn save_settings(&self, settings: Settings) -> Result<()> {
        let session = self.session.lock().await;
        if session.is_some() {
            bail!("Stop listening before changing settings");
        }
        // A recording session is already rejected above, so this lock is held by
        // another process using the same data directory.
        let _capture_lock = self.capture_lock().map_err(|_| {
            anyhow::anyhow!(
                "Another {} process is already listening in this data directory",
                config::PRODUCT_NAME
            )
        })?;
        models::speech_path(&self.paths, &settings.speech_model)?;
        models::cleanup_location(&self.paths, &settings)?;
        self.paths.save(&settings)?;
        let old = self.settings();
        if old.filter_model != settings.filter_model
            || old.cleanup_source != settings.cleanup_source
            || old.cleanup_backend != settings.cleanup_backend
            || old.mlx_python != settings.mlx_python
        {
            let cleanup_engine = self.cleanup_engine.clone();
            tokio::task::spawn_blocking(move || cleanup_engine.lock().unwrap().unload()).await?;
        }
        self.engine.lock().unwrap().unload();
        *self.settings.lock().unwrap() = settings;
        Ok(())
    }
    pub async fn status(&self) -> Status {
        Status {
            recording: self.session.lock().await.is_some(),
            processing: self.processing.load(std::sync::atomic::Ordering::Relaxed),
            data_directory: self.paths.root.to_string_lossy().into_owned(),
        }
    }
    pub fn history(&self, query: &str, limit: u32) -> Result<Vec<Transcript>> {
        history::History::open(&self.paths)?.search(query, limit)
    }
    pub fn export(&self) -> Result<Vec<Transcript>> {
        history::History::open(&self.paths)?.all()
    }
    pub fn note(&self, text: &str) -> Result<Transcript> {
        if text.trim().is_empty() {
            bail!("Note cannot be empty");
        }
        let entry = Transcript {
            id: uuid::Uuid::new_v4().to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            raw: text.into(),
            refined: text.into(),
            speech_model: "manual-note".into(),
            microphone: None,
            filter_model: None,
            duration_seconds: 0.0,
            audio_path: None,
            filter_error: None,
        };
        history::History::open(&self.paths)?.insert(&entry)?;
        self.log_processed(&entry);
        let _ = self.events.send(Event::Transcript {
            entry: entry.clone(),
        });
        Ok(entry)
    }
    pub fn delete(&self, id: &str) -> Result<()> {
        // Derive retained audio path from UUID, never from an arbitrary stored path.
        let id = uuid::Uuid::parse_str(id)?.to_string();
        history::History::open(&self.paths)?.delete(&id)?;
        let path = self.paths.root.join("audio").join(format!("{id}.wav"));
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }
    pub async fn unload(&self) -> Result<()> {
        let engine = self.engine.clone();
        tokio::task::spawn_blocking(move || engine.lock().unwrap().unload()).await?;
        let cleanup_engine = self.cleanup_engine.clone();
        tokio::task::spawn_blocking(move || cleanup_engine.lock().unwrap().unload()).await?;
        let _ = self.events.send(Event::Unloaded);
        Ok(())
    }
    pub async fn process(
        &self,
        samples: Vec<f32>,
        settings: Settings,
    ) -> Result<Option<Transcript>> {
        let started_at = chrono::Utc::now()
            - chrono::Duration::milliseconds((samples.len() as f64 / 16.0) as i64);
        self.process_at(
            samples,
            settings,
            started_at,
            uuid::Uuid::new_v4().to_string(),
        )
        .await
    }
    pub async fn refine(&self, text: &str, settings: Settings) -> Result<String> {
        let cleanup_engine = self.cleanup_engine.clone();
        let cleanup_paths = self.paths.clone();
        let text = text.to_owned();
        tokio::task::spawn_blocking(move || {
            cleanup_engine
                .lock()
                .unwrap()
                .refine(&cleanup_paths, &settings, &text)
        })
        .await?
    }
    async fn process_at(
        &self,
        samples: Vec<f32>,
        settings: Settings,
        started_at: chrono::DateTime<chrono::Utc>,
        id: String,
    ) -> Result<Option<Transcript>> {
        self.activity(format!(
            "Processing segment {id}: {:.1}s of audio, speech start {started_at}, model {}",
            samples.len() as f64 / 16000.0,
            settings.speech_model
        ));
        self.processing
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = self.events.send(Event::Status {
            recording: self.recording.load(std::sync::atomic::Ordering::Relaxed),
            processing: true,
        });
        let result = self.process_inner(samples, settings, started_at, id).await;
        if let Err(error) = &result {
            self.activity(format!("Processing failed: {error:#}"));
        }
        self.processing
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let _ = self.events.send(Event::Status {
            recording: self.recording.load(std::sync::atomic::Ordering::Relaxed),
            processing: false,
        });
        result
    }
    async fn process_inner(
        &self,
        samples: Vec<f32>,
        settings: Settings,
        created_at: chrono::DateTime<chrono::Utc>,
        id: String,
    ) -> Result<Option<Transcript>> {
        let duration = samples.len() as f64 / 16000.0;
        let engine = self.engine.clone();
        let opts = settings.clone();
        let (raw, samples) = tokio::task::spawn_blocking(move || {
            if !audio::has_speech(&samples, &opts)? {
                return anyhow::Ok((None, samples));
            }
            let raw = engine.lock().unwrap().transcribe(&samples, &opts)?;
            anyhow::Ok((Some(raw), samples))
        })
        .await??;
        let Some(raw) = raw else {
            self.activity(format!(
                "Segment {id}: no voice detected; transcription skipped"
            ));
            return Ok(None);
        };
        if raw.is_empty() {
            self.activity(format!(
                "Segment {id}: no speech text recognized; no note saved"
            ));
            return Ok(None);
        }
        if content::should_skip(&raw, settings.short_transcript_policy) {
            self.activity(format!(
                "Segment {id}: short transcript skipped; no note or audio saved"
            ));
            return Ok(None);
        }
        if settings.filter_enabled {
            self.activity(format!(
                "Segment {id}: cleaning up transcript with {} (saved text language: {})",
                settings.filter_model, settings.output_language
            ));
        }
        let refined = self.refine(&raw, settings.clone()).await;
        let (refined, filter_error) = match refined {
            Ok(text) => (text, None),
            Err(e) => {
                let message = e.to_string();
                let _ = self.events.send(Event::Error {
                    message: message.clone(),
                });
                (raw.clone(), Some(message))
            }
        };
        if filter_error.is_none()
            && content::should_skip(&refined, settings.short_transcript_policy)
        {
            self.activity(format!(
                "Segment {id}: cleaned transcript discarded; no note or audio saved"
            ));
            return Ok(None);
        }
        // Queued recording jobs remain recoverable until this entire operation succeeds.
        // Save original and cleaned text together after the model decides what remains.
        let audio_path = if settings.retain_audio {
            let path = self.paths.root.join("audio").join(format!("{id}.wav"));
            audio::write_wav(&path, &samples)?;
            Some(path.to_string_lossy().into_owned())
        } else {
            None
        };
        let entry = Transcript {
            id,
            created_at: created_at.to_rfc3339(),
            refined,
            raw,
            speech_model: settings.speech_model.clone(),
            microphone: settings.microphone.clone(),
            filter_model: settings
                .filter_enabled
                .then_some(settings.filter_model.clone()),
            duration_seconds: duration,
            audio_path,
            filter_error,
        };
        history::History::open(&self.paths)?.insert(&entry)?;
        self.log_processed(&entry);
        let _ = self.events.send(Event::Transcript {
            entry: entry.clone(),
        });
        Ok(Some(entry))
    }
    fn log_processed(&self, entry: &Transcript) {
        match logging::ProcessingLog::new(&self.paths, entry) {
            Ok(log) => {
                if let Err(error) = log.append(&self.paths) {
                    let _ = self.events.send(Event::Error {
                        message: format!("Text was saved, but the processing log could not be written: {error:#}"),
                    });
                }
                let _ = self.events.send(Event::Processed { log });
            }
            Err(error) => {
                let _ = self.events.send(Event::Error {
                    message: format!(
                        "Text was saved, but the processing log could not be created: {error:#}"
                    ),
                });
            }
        }
    }
    fn activity(&self, message: impl Into<String>) {
        let log = logging::ActivityLog::new(message);
        if let Err(error) = log.append(&self.paths) {
            let _ = self.events.send(Event::Error {
                message: format!("Cannot write activity log: {error:#}"),
            });
        }
        let _ = self.events.send(Event::Activity { log });
    }
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        let mut session = self.session.lock().await;
        if session.is_some() {
            bail!("Already listening");
        }
        let settings = self.settings();
        if !models::speech_path(&self.paths, &settings.speech_model)?.exists() {
            bail!("Download {} before listening", settings.speech_model);
        }
        // Only one process may own microphone capture for this data directory.
        let recording_lock = self.capture_lock()?;
        let mut inputs = settings.selected_microphones();
        let mut segmenters = inputs
            .iter()
            .map(|_| audio::Segmenter::new(settings.clone()))
            .collect::<Result<Vec<_>>>()?;
        let (audio_tx, mut audio_rx) = mpsc::channel(256);
        let (err_tx, mut err_rx) = mpsc::unbounded_channel();
        let mut captures = Vec::new();
        for (source, microphone) in inputs.iter().enumerate() {
            captures.push(audio::Capture::start_tagged(
                microphone.clone(),
                source,
                audio_tx.clone(),
                err_tx.clone(),
            )?);
        }
        if settings.record_device_audio {
            let source = inputs.len();
            captures.push(audio::Capture::start_device_output(
                source,
                audio_tx.clone(),
                err_tx.clone(),
            )?);
            inputs.push(Some("Device audio · system output".into()));
            segmenters.push(audio::Segmenter::new(settings.clone())?);
        }
        drop(audio_tx);
        let descriptions: Vec<_> = captures
            .iter()
            .map(|capture| capture.description.clone())
            .collect();
        self.activity(format!(
            "Audio capture started: {}. Silero speech confidence: {:.2}; minimum RMS: {:.4}",
            descriptions.join(" + "),
            settings.vad_threshold,
            settings.speech_threshold
        ));
        self.activity(format!(
            "Activity logs: {}; saved-text logs: {}",
            self.paths.logs_dir.join("activity.jsonl").display(),
            self.paths.logs_dir.join("processing.jsonl").display()
        ));
        self.recording
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let (stop, mut stopped) = oneshot::channel();
        let service = self.clone();
        let done = tokio::spawn(async move {
            let _recording_lock = recording_lock;
            let (segments_tx, mut segments_rx) = mpsc::unbounded_channel::<std::path::PathBuf>();
            match queue::pending(&service.paths) {
                Ok(paths) => {
                    for path in paths {
                        let _ = segments_tx.send(path);
                    }
                }
                Err(e) => {
                    let _ = service.events.send(Event::Error {
                        message: e.to_string(),
                    });
                }
            }
            let worker_service = service.clone();
            let worker = tokio::spawn(async move {
                while let Some(path) = segments_rx.recv().await {
                    let result = async {
                        let job = queue::load(&path)?;
                        worker_service
                            .process_at(
                                job.segment.samples,
                                job.settings,
                                job.segment.started_at,
                                job.id,
                            )
                            .await?;
                        std::fs::remove_file(&path)?;
                        anyhow::Ok(())
                    }
                    .await;
                    if let Err(e) = result {
                        let _ = worker_service.events.send(Event::Error {
                            message: format!(
                                "{e}. Speech remains queued in {} for retry on next start.",
                                path.display()
                            ),
                        });
                    }
                }
            });
            let mut last_level = std::time::Instant::now();
            let mut input_samples = 0usize;
            let mut peak_level = 0.0f32;
            let mut input_issue_reported = false;
            let mut heartbeat = tokio::time::interval_at(
                tokio::time::Instant::now() + std::time::Duration::from_secs(10),
                std::time::Duration::from_secs(10),
            );
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    _ = heartbeat.tick() => {
                        service.activity(format!("Audio input: {:.1}s received from {} inputs in the last 10s; peak RMS {:.6e}, minimum RMS {:.6e}; voice confidence {:.2}, VAD threshold {:.2}; {}", input_samples as f64 / 16000.0, segmenters.len(), peak_level, settings.speech_threshold, segmenters.iter().map(|s| s.last_speech_probability()).fold(0.0f32, f32::max), settings.vad_threshold, if segmenters.iter().any(|s| s.is_active()) { "capturing speech" } else { "waiting for speech" }));
                        if input_samples == 0 || peak_level == 0.0 {
                            if !input_issue_reported {
                                service.activity(if input_samples == 0 {
                                    "Microphone stream is open but no audio samples arrived. Check the selected input and reconnect it if needed."
                                } else {
                                    "Microphone buffers contain only zero-valued samples. Check microphone permission for the app hosting this terminal, and check Sound > Input in system settings. On macOS, an embedded terminal uses its host app (for example T3 Code). Restart the host app after granting access."
                                });
                                input_issue_reported = true;
                            }
                        } else {
                            input_issue_reported = false;
                        }
                        input_samples = 0;
                        peak_level = 0.0;
                    },
                    Some(message) = err_rx.recv() => { let _ = service.events.send(Event::Error { message }); },
                    samples = audio_rx.recv() => {
                        let Some(input) = samples else { break };
                        let source = input.source;
                        let samples = input.samples;
                        let segmenter = &mut segmenters[source];
                        let mut source_settings = settings.clone();
                        source_settings.microphone = Some(inputs[source].clone().unwrap_or_else(|| descriptions[source].clone()));
                        source_settings.microphones.clear();
                        input_samples += samples.len();
                        peak_level = peak_level.max(audio::rms(&samples));
                        if last_level.elapsed().as_millis() >= 100 {
                            let _ = service.events.send(Event::Level { value: audio::rms(&samples) });
                            last_level = std::time::Instant::now();
                        }
                        let was_active = segmenter.is_active();
                        match segmenter.push(&samples) {
                            Ok(segments) => {
                                if !was_active && segmenter.is_active() {
                                    service.activity(format!("Speech detected on {}; collecting audio until a pause", descriptions[source]));
                                }
                                for segment in segments {
                                    service.enqueue(segment, &source_settings, &segments_tx);
                                }
                            }
                            Err(error) => {
                                let _ = service.events.send(Event::Error { message: format!("Speech detection failed: {error:#}") });
                                break;
                            }
                        }
                    }
                }
            }
            for capture in &mut captures {
                capture.stop();
            }
            while let Ok(input) = audio_rx.try_recv() {
                let segmenter = &mut segmenters[input.source];
                let mut source_settings = settings.clone();
                source_settings.microphone = Some(
                    inputs[input.source]
                        .clone()
                        .unwrap_or_else(|| descriptions[input.source].clone()),
                );
                source_settings.microphones.clear();
                match segmenter.push(&input.samples) {
                    Ok(segments) => {
                        for segment in segments {
                            service.enqueue(segment, &source_settings, &segments_tx);
                        }
                    }
                    Err(error) => {
                        let _ = service.events.send(Event::Error {
                            message: format!("Speech detection failed: {error:#}"),
                        });
                        break;
                    }
                }
            }
            for (source, segmenter) in segmenters.iter_mut().enumerate() {
                let mut source_settings = settings.clone();
                source_settings.microphone = Some(
                    inputs[source]
                        .clone()
                        .unwrap_or_else(|| descriptions[source].clone()),
                );
                source_settings.microphones.clear();
                match segmenter.flush() {
                    Ok(Some(segment)) => service.enqueue(segment, &source_settings, &segments_tx),
                    Ok(None) => {}
                    Err(error) => {
                        let _ = service.events.send(Event::Error {
                            message: format!("Speech detection failed: {error:#}"),
                        });
                    }
                }
            }
            drop(segments_tx);
            let _ = worker.await;
            service
                .recording
                .store(false, std::sync::atomic::Ordering::Relaxed);
            service.activity("Microphone stopped; queued processing finished");
            let _ = service.events.send(Event::Status {
                recording: false,
                processing: false,
            });
        });
        *session = Some(Session { stop, done });
        let _ = self.events.send(Event::Status {
            recording: true,
            processing: false,
        });
        Ok(())
    }
    pub async fn stop(&self) -> Result<()> {
        let mut session = self.session.lock().await;
        if let Some(active) = session.take() {
            let _ = active.stop.send(());
            active.done.await?;
        }
        Ok(())
    }
    fn enqueue(
        &self,
        segment: audio::Segment,
        settings: &Settings,
        sender: &mpsc::UnboundedSender<std::path::PathBuf>,
    ) {
        let duration = segment.samples.len() as f64 / 16000.0;
        match queue::enqueue(&self.paths, segment, settings.clone()) {
            Ok(path) => {
                self.activity(format!(
                    "Queued {duration:.1}s of audio: {}",
                    path.display()
                ));
                let _ = sender.send(path);
            }
            Err(e) => {
                let _ = self.events.send(Event::Error {
                    message: format!("Could not queue speech to disk: {e}"),
                });
            }
        }
    }
    fn capture_lock(&self) -> Result<std::fs::File> {
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.paths.root.join(".capture.lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| {
            anyhow::anyhow!(
                "Another {} process is already listening",
                config::PRODUCT_NAME
            )
        })?;
        Ok(lock)
    }
}
