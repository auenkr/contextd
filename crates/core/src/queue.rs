//! Disk-backed speech backlog: slow inference does not discard completed segments.
use crate::{audio::Segment, Paths, Settings};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub segment: Segment,
    pub settings: Settings,
}

pub fn enqueue(paths: &Paths, segment: Segment, settings: Settings) -> Result<PathBuf> {
    let id = uuid::Uuid::new_v4().to_string();
    let filename = format!("{}-{}", segment.started_at.timestamp_millis(), id);
    let path = paths.root.join("pending").join(format!("{filename}.json"));
    let temporary = path.with_extension("part");
    std::fs::write(
        &temporary,
        serde_json::to_vec(&Job {
            id,
            segment,
            settings,
        })?,
    )?;
    std::fs::rename(temporary, &path)?;
    Ok(path)
}

pub fn pending(paths: &Paths) -> Result<Vec<PathBuf>> {
    let mut files = std::fs::read_dir(paths.root.join("pending"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}
pub fn load(path: &std::path::Path) -> Result<Job> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queued_speech_survives_restart_with_original_time_and_model() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_directory(dir.path().to_owned());
        std::fs::create_dir(paths.root.join("pending")).unwrap();
        let started_at = chrono::Utc::now();
        let path = enqueue(
            &paths,
            Segment {
                samples: vec![0.1; 1600],
                started_at,
            },
            Settings {
                output_language: "en".into(),
                filter_model: "qwen3.5:0.8b".into(),
                ..Settings::default()
            },
        )
        .unwrap();
        assert_eq!(pending(&paths).unwrap(), vec![path.clone()]);
        let job = load(&path).unwrap();
        assert_eq!(job.segment.started_at, started_at);
        assert_eq!(job.segment.samples.len(), 1600);
        assert_eq!(job.settings.speech_model, "parakeet-unified-en-0.6b");
        assert_eq!(job.settings.output_language, "en");
        assert!(uuid::Uuid::parse_str(&job.id).is_ok());
        std::fs::remove_file(path).unwrap();
        assert!(pending(&paths).unwrap().is_empty());
    }
}
