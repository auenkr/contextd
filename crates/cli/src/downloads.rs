use anyhow::Result;
use contextd_core::{models, Event, Paths, Service};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::watch;

/// Several models can download side by side; the rest wait for a free slot.
const MAX_CONCURRENT: usize = 2;

#[derive(Clone, Debug, Default)]
struct Outcome {
    finished: bool,
    error: Option<String>,
}

struct Entry {
    progress: watch::Receiver<Outcome>,
    task: tokio::task::JoinHandle<Result<()>>,
}

struct Registry {
    running: usize,
    entries: HashMap<String, Entry>,
    waiting: VecDeque<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub completed: u64,
    pub total: u64,
    pub finished: bool,
    pub error: Option<String>,
}

impl Progress {
    pub fn ratio(&self) -> f64 {
        if self.finished && self.error.is_none() {
            1.0
        } else {
            (self.percent() / 100.0).clamp(0.0, 1.0)
        }
    }
    pub fn title(&self) -> &'static str {
        if self.error.is_some() {
            "Download failed"
        } else if self.finished {
            "Downloaded"
        } else {
            "Downloading"
        }
    }
    pub fn label(&self, id: &str, queued: bool) -> String {
        if let Some(error) = &self.error {
            format!("{id}: failed · {error}")
        } else if self.finished {
            format!("{id}: downloaded · ready to use")
        } else if queued {
            format!("{id}: waiting for a free download slot")
        } else if self.total > 0 && self.completed >= self.total {
            format!("{id}: 100% · finalizing download…")
        } else {
            format!(
                "{id}: {:.0}% · {:.1} / {:.1} MB",
                self.percent(),
                self.completed as f64 / 1e6,
                self.total as f64 / 1e6
            )
        }
    }

    /// Whole percent of a known-size transfer, zero when the length is unknown.
    pub fn percent(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            (self.completed as f64 / self.total as f64) * 100.0
        }
    }
}

#[derive(Clone)]
pub struct Downloads {
    inner: Arc<Inner>,
}

struct Inner {
    service: Arc<Service>,
    runtime: tokio::runtime::Handle,
    registry: Mutex<Registry>,
    /// Newest byte counts per model, so every screen reports identical progress.
    progress: Mutex<HashMap<String, Progress>>,
}

async fn next_event(events: &mut tokio::sync::broadcast::Receiver<Event>) -> Option<Event> {
    loop {
        match events.recv().await {
            Ok(event) => return Some(event),
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
        }
    }
}

impl Downloads {
    pub fn new(service: Arc<Service>, runtime: tokio::runtime::Handle) -> Arc<Self> {
        let downloads = Arc::new(Self {
            inner: Arc::new(Inner {
                service: service.clone(),
                runtime: runtime.clone(),
                registry: Mutex::new(Registry {
                    running: 0,
                    entries: HashMap::new(),
                    waiting: VecDeque::new(),
                }),
                progress: Mutex::new(HashMap::new()),
            }),
        });
        downloads.mirror_events();
        downloads
    }

    /// Mirror download events so the model selection screen and the dashboard agree.
    fn mirror_events(self: &Arc<Self>) {
        let downloads = self.clone();
        let mut events = self.inner.service.events.subscribe();
        self.inner.runtime.spawn(async move {
            while let Some(event) = next_event(&mut events).await {
                if let Event::Download {
                    id,
                    completed,
                    total,
                } = event
                {
                    downloads.inner.progress.lock().unwrap().insert(
                        id,
                        Progress {
                            completed,
                            total,
                            ..Progress::default()
                        },
                    );
                }
            }
        });
    }

    pub fn paths(&self) -> &Paths {
        &self.inner.service.paths
    }

    /// Every transfer with byte counts, downloading or queued first.
    pub fn snapshot(&self) -> Vec<(String, Progress)> {
        let registry = self.inner.registry.lock().unwrap();
        let progress = self.inner.progress.lock().unwrap();
        let ordered: Vec<String> = registry
            .waiting
            .iter()
            .cloned()
            .chain(registry.entries.keys().cloned())
            .collect();
        let mut seen = std::collections::HashSet::new();
        let mut snapshot: Vec<_> = ordered
            .into_iter()
            .filter(|id| seen.insert(id.clone()))
            .map(|id| {
                let mut bytes = progress.get(&id).cloned().unwrap_or_default();
                if let Some(entry) = registry.entries.get(&id) {
                    let outcome = entry.progress.borrow();
                    bytes.finished = outcome.finished;
                    bytes.error = outcome.error.clone();
                }
                (id, bytes)
            })
            .collect();
        snapshot.sort_by(|a, b| a.1.finished.cmp(&b.1.finished).then(a.0.cmp(&b.0)));
        snapshot
    }

    pub fn queued(&self, id: &str) -> bool {
        self.inner
            .registry
            .lock()
            .unwrap()
            .waiting
            .contains(&models::canonical_cleanup_id(id).to_owned())
    }

    pub fn is_installed(&self, id: &str) -> bool {
        models::is_installed(self.paths(), id)
    }

    /// Start a download if it is not already cached; repeated calls join the same transfer.
    pub fn enqueue(self: &Arc<Self>, id: &str) {
        let id = models::canonical_cleanup_id(id).to_owned();
        if self.is_installed(&id) {
            self.inner.progress.lock().unwrap().remove(&id);
            return;
        }
        {
            let mut registry = self.inner.registry.lock().unwrap();
            if registry
                .entries
                .get(&id)
                .is_some_and(|entry| entry.progress.borrow().finished)
            {
                registry.entries.remove(&id);
                self.inner.progress.lock().unwrap().remove(&id);
            }
            if registry.entries.contains_key(&id) || registry.waiting.contains(&id) {
                return;
            }
        }
        self.inner.registry.lock().unwrap().waiting.push_back(id);
        self.pump();
    }

    fn pump(self: &Arc<Self>) {
        loop {
            let next = {
                let mut registry = self.inner.registry.lock().unwrap();
                if registry.running >= MAX_CONCURRENT {
                    break;
                }
                match registry.waiting.pop_front() {
                    Some(id) => {
                        registry.running += 1;
                        id
                    }
                    None => break,
                }
            };
            let downloads = self.clone();
            let service = self.inner.service.clone();
            let id = next.clone();
            let (progress, tracker) = watch::channel(Outcome::default());
            let task = self.inner.runtime.spawn(async move {
                let result = models::download(&service.paths, &id, &service.events).await;
                let outcome = match &result {
                    Ok(()) => Outcome {
                        finished: true,
                        error: None,
                    },
                    Err(error) => Outcome {
                        finished: true,
                        error: Some(format!("{error:#}")),
                    },
                };
                let _ = progress.send(outcome);
                // Free the slot and let the next queued model start.
                downloads.advance();
                result
            });
            self.inner.registry.lock().unwrap().entries.insert(
                next,
                Entry {
                    progress: tracker,
                    task,
                },
            );
        }
    }

    fn advance(self: &Arc<Self>) {
        {
            let mut registry = self.inner.registry.lock().unwrap();
            registry.running = registry.running.saturating_sub(1);
        }
        self.pump();
    }

    /// Wait for a model to be cached, starting its download when necessary.
    pub async fn wait(self: &Arc<Self>, id: &str) -> Result<()> {
        let id = models::canonical_cleanup_id(id).to_owned();
        if self.is_installed(&id) {
            return Ok(());
        }
        self.enqueue(&id);
        let tracker = self
            .inner
            .registry
            .lock()
            .unwrap()
            .entries
            .get(&id)
            .map(|entry| entry.progress.clone());
        // An already-cached model has no tracker; the check above returns first.
        let Some(mut tracker) = tracker else {
            return Ok(());
        };
        tracker
            .wait_for(|outcome| outcome.finished)
            .await
            .map_err(|error| anyhow::anyhow!("Download tracking stopped: {error}"))?;
        let outcome = tracker.borrow().clone();
        match outcome.error {
            Some(error) => Err(anyhow::anyhow!(error)),
            None => Ok(()),
        }
    }

    /// Abandon a transfer, discard its partial file, and keep a later retry possible.
    pub fn cancel(self: &Arc<Self>, id: &str) {
        let id = models::canonical_cleanup_id(id).to_owned();
        {
            let mut registry = self.inner.registry.lock().unwrap();
            registry.waiting.retain(|queued| queued != &id);
            if let Some(entry) = registry.entries.remove(&id) {
                entry.task.abort();
                if !entry.progress.borrow().finished {
                    registry.running = registry.running.saturating_sub(1);
                }
            }
        }
        let _ = models::remove_partial(self.paths(), &id);
        self.pump();
    }

    pub fn cancel_all(self: &Arc<Self>) {
        let ids: Vec<String> = {
            let registry = self.inner.registry.lock().unwrap();
            registry.entries.keys().cloned().collect()
        };
        for id in ids {
            self.cancel(&id);
        }
    }

    /// Stop tracking a model that finished or was deleted, so it can download again.
    pub fn forget(&self, id: &str) {
        let id = models::canonical_cleanup_id(id).to_owned();
        {
            let mut registry = self.inner.registry.lock().unwrap();
            registry.waiting.retain(|queued| queued != &id);
            registry.entries.remove(&id);
        }
        self.inner.progress.lock().unwrap().remove(&id);
    }

    pub fn any_active(&self) -> bool {
        let registry = self.inner.registry.lock().unwrap();
        registry.running > 0 || !registry.waiting.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn progress_listener_survives_lag_and_tracks_later_downloads() {
        let (sender, mut receiver) = tokio::sync::broadcast::channel(1);
        for completed in 0..3 {
            sender
                .send(Event::Download {
                    id: "test".into(),
                    completed,
                    total: 3,
                })
                .unwrap();
        }
        assert!(matches!(
            next_event(&mut receiver).await,
            Some(Event::Download { completed: 2, .. })
        ));
        sender
            .send(Event::Download {
                id: "next".into(),
                completed: 1,
                total: 5,
            })
            .unwrap();
        assert!(
            matches!(next_event(&mut receiver).await, Some(Event::Download { id, .. }) if id == "next")
        );
    }

    #[test]
    fn full_transfer_is_not_installed_until_download_finishes() {
        let mut progress = Progress {
            completed: 100,
            total: 100,
            ..Progress::default()
        };
        assert_eq!(progress.title(), "Downloading");
        assert!(progress.label("test", false).contains("finalizing"));
        progress.finished = true;
        assert_eq!(progress.title(), "Downloaded");
        progress.completed = 0;
        assert_eq!(progress.ratio(), 1.0);
        assert_eq!(
            progress.label("test", false),
            "test: downloaded · ready to use"
        );
        progress.error = Some("disk full".into());
        assert_eq!(progress.title(), "Download failed");
        assert!(progress.label("test", false).contains("disk full"));
    }
}
