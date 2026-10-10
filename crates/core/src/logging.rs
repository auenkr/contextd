//! Persistent completion logs for every adapter using the core service.
use crate::{history, Paths, Transcript};
use anyhow::Result;
use fs2::FileExt;
use serde::Serialize;
use std::io::Write;

#[derive(Clone, Debug, Serialize)]
pub struct ActivityLog {
    pub timestamp: String,
    pub message: String,
}

impl ActivityLog {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            timestamp: chrono::Local::now().to_rfc3339(),
            message: message.into(),
        }
    }
    pub fn append(&self, paths: &Paths) -> Result<()> {
        append(paths, "activity.jsonl", self)
    }
}

fn append(paths: &Paths, filename: &str, value: &impl Serialize) -> Result<()> {
    let directory = paths.logs_dir.clone();
    std::fs::create_dir_all(&directory)?;
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(filename))?;
    file.lock_exclusive()?;
    file.write_all(&line)?;
    file.flush()?;
    file.unlock()?;
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
pub struct ProcessingLog {
    pub processed_at: String,
    pub started_at: String,
    pub id: String,
    pub raw_text: String,
    pub saved_text: String,
    pub note_path: String,
    pub audio_path: Option<String>,
    pub history_path: String,
    pub filter_error: Option<String>,
}

impl ProcessingLog {
    pub fn new(paths: &Paths, entry: &Transcript) -> Result<Self> {
        // Resolve relative --data-dir values so logs identify the saved files
        // even when read from a different working directory later.
        let absolute_paths = Paths::in_directory(paths.root.canonicalize()?);
        Ok(Self {
            processed_at: chrono::Local::now().to_rfc3339(),
            started_at: entry.created_at.clone(),
            id: entry.id.clone(),
            raw_text: entry.raw.clone(),
            saved_text: entry.refined.clone(),
            note_path: history::note_path(&absolute_paths, &entry.created_at)?
                .to_string_lossy()
                .into_owned(),
            audio_path: entry.audio_path.as_ref().map(|_| {
                absolute_paths
                    .root
                    .join("audio")
                    .join(format!("{}.wav", entry.id))
                    .to_string_lossy()
                    .into_owned()
            }),
            history_path: absolute_paths
                .root
                .join("history.sqlite3")
                .to_string_lossy()
                .into_owned(),
            filter_error: entry.filter_error.clone(),
        })
    }

    pub fn append(&self, paths: &Paths) -> Result<()> {
        // Separate CLI processes can save manual notes while capture is active.
        append(paths, "processing.jsonl", self)
    }
}
