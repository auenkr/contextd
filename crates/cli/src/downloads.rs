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
}

impl Progress {
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
            while let Ok(event) = events.recv().await {
                if let Event::Download {
                    id,
                    completed,
                    total,
                } = event
                {
                    downloads
                        .inner
                        .progress
                        .lock()
                        .unwrap()
                        .insert(id, Progress { completed, total });
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
        ordered
            .into_iter()
            .filter(|id| seen.insert(id.clone()))
            .map(|id| {
                let bytes = progress.get(&id).cloned().unwrap_or_default();
                (id, bytes)
            })
            .collect()
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
            let registry = self.inner.registry.lock().unwrap();
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
                        error: Some(error.to_string()),
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
                registry.running = registry.running.saturating_sub(1);
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
