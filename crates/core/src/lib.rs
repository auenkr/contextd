pub mod audio;
pub mod config;
pub mod context;
pub mod explore;
pub mod history;
pub mod inference;
pub mod logging;
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
        let paths = Paths::discover()?;
        let settings = paths.load()?;
        settings.validate()?;
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
    pub async fn save_settings(&self, settings: Settings) -> Result<()> {
        let session = self.session.lock().await;
        if session.is_some() {
            bail!("Stop listening before changing settings");
        }
        let _capture_lock = self.capture_lock().map_err(|_| {
            anyhow::anyhow!("Stop listening before changing settings; another process is recording")
        })?;
        models::speech_path(&self.paths, &settings.speech_model)?;
        self.paths.save(&settings)?;
        let old = self.settings();
        if old.filter_model != settings.filter_model {
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
        let audio_path = if settings.retain_audio {
            let path = self.paths.root.join("audio").join(format!("{id}.wav"));
            audio::write_wav(&path, &samples)?;
            Some(path.to_string_lossy().into_owned())
        } else {
            None
        };
        let engine = self.engine.clone();
        let opts = settings.clone();
        let raw =
            tokio::task::spawn_blocking(move || engine.lock().unwrap().transcribe(&samples, &opts))
                .await??;
        if raw.is_empty() {
            self.activity(format!(
                "Segment {id}: no speech text recognized; no note saved"
            ));
            return Ok(None);
        }
        // Persist raw speech BEFORE cleanup, so a failed/slow LLM cannot lose the transcript.
        let mut entry = Transcript {
            id,
            created_at: created_at.to_rfc3339(),
            refined: raw.clone(),
            raw: raw.clone(),
            speech_model: settings.speech_model.clone(),
            filter_model: settings
                .filter_enabled
                .then_some(settings.filter_model.clone()),
            duration_seconds: duration,
            audio_path,
            filter_error: None,
        };
        history::History::open(&self.paths)?.insert(&entry)?;
        self.activity(format!(
            "Segment {}: transcription saved to {}; text: {}",
            entry.id,
            history::note_path(&self.paths, &entry.created_at)?.display(),
            serde_json::to_string(&raw)?
        ));
        if settings.filter_enabled {
            self.activity(format!(
                "Segment {}: cleaning up transcript with {}",
                entry.id, settings.filter_model
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
        entry.refined = refined;
        entry.filter_error = filter_error;
        if settings.filter_enabled {
            history::History::open(&self.paths)?.update(&entry)?;
        }
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
        let (audio_tx, mut audio_rx) = mpsc::channel(256);
        let (err_tx, mut err_rx) = mpsc::unbounded_channel();
        let mut capture = audio::Capture::start(settings.microphone.clone(), audio_tx, err_tx)?;
        self.activity(format!(
            "Microphone started: {}. Speech threshold: {:.4}",
            capture.description, settings.speech_threshold
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
            let mut segmenter = audio::Segmenter::new(settings.clone());
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
                        service.activity(format!("Microphone input: {:.1}s received in the last 10s; peak RMS {:.6e}, speech threshold {:.6e}; {}", input_samples as f64 / 16000.0, peak_level, settings.speech_threshold, if segmenter.is_active() { "capturing speech" } else { "waiting for speech" }));
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
                        let Some(samples) = samples else { break };
                        input_samples += samples.len();
                        peak_level = peak_level.max(audio::rms(&samples));
                        if last_level.elapsed().as_millis() >= 100 {
                            let _ = service.events.send(Event::Level { value: audio::rms(&samples) });
                            last_level = std::time::Instant::now();
                        }
                        let was_active = segmenter.is_active();
                        if let Some(segment) = segmenter.push(&samples) {
                            service.enqueue(segment, &settings, &segments_tx);
                        } else if !was_active && segmenter.is_active() {
                            service.activity("Speech detected; collecting audio until a pause or the segment limit");
                        }
                    }
                }
            }
            capture.stop();
            while let Ok(samples) = audio_rx.try_recv() {
                if let Some(segment) = segmenter.push(&samples) {
                    service.enqueue(segment, &settings, &segments_tx);
                }
            }
            if let Some(segment) = segmenter.flush() {
                service.enqueue(segment, &settings, &segments_tx);
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
