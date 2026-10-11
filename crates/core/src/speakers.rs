//! Persistent voice profiles and local speaker diarization.
use crate::{audio, Paths, Settings};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Speaker {
    pub id: String,
    pub name: String,
    pub named: bool,
    pub sample_path: String,
    pub created_at: String,
    pub sample_seconds: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpeakerTurn {
    pub speaker_id: Option<String>,
    pub speaker_name: String,
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub raw: String,
    pub refined: String,
    #[serde(default)]
    pub overlap: bool,
}
#[derive(Deserialize)]
pub struct Detection {
    pub turns: Vec<DetectedTurn>,
    pub voices: Vec<Voice>,
}
#[derive(Deserialize)]
pub struct DetectedTurn {
    pub start: f64,
    pub end: f64,
    pub local_speaker: i32,
}
#[derive(Deserialize)]
pub struct Voice {
    pub local_speaker: i32,
    pub embedding: Vec<f32>,
    pub ranges: Vec<[f64; 2]>,
}

pub fn connection(paths: &Paths) -> Result<Connection> {
    let connection = Connection::open(paths.root.join("history.sqlite3"))?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS speakers (
        id TEXT PRIMARY KEY, name TEXT NOT NULL, named INTEGER NOT NULL,
        document TEXT NOT NULL, embedding TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS speaker_sync (day TEXT NOT NULL, speaker_id TEXT NOT NULL, old_name TEXT NOT NULL, new_name TEXT NOT NULL, PRIMARY KEY(day,speaker_id));",
    )?;
    Ok(connection)
}
pub fn list(paths: &Paths) -> Result<Vec<Speaker>> {
    let connection = connection(paths)?;
    let mut statement = connection.prepare("SELECT document FROM speakers ORDER BY rowid")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
}
pub(crate) fn resolve_names(connection: &Connection, entry: &mut crate::Transcript) -> Result<()> {
    use rusqlite::OptionalExtension;
    for turn in &mut entry.speaker_turns {
        if let Some(id) = &turn.speaker_id {
            if let Some(name) = connection
                .query_row("SELECT name FROM speakers WHERE id=?1", [id], |r| {
                    r.get::<_, String>(0)
                })
                .optional()?
            {
                turn.speaker_name = name;
            }
        }
    }
    Ok(())
}

pub fn rename(paths: &Paths, settings: &Settings, id: &str, name: &str) -> Result<()> {
    crate::history::History::open(paths)?;
    let id = uuid::Uuid::parse_str(id)?.to_string();
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(|c| c.is_control()) {
        bail!("Speaker name must be 1–80 characters with no control characters");
    }
    let mut connection = connection(paths)?;
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let document: String = tx
        .query_row("SELECT document FROM speakers WHERE id=?1", [&id], |r| {
            r.get(0)
        })
        .context("Speaker not found")?;
    let mut speaker: Speaker = serde_json::from_str(&document)?;
    let old = speaker.name.clone();
    speaker.name = name.into();
    speaker.named = true;
    tx.execute(
        "UPDATE speakers SET name=?2,named=1,document=?3 WHERE id=?1",
        params![id, name, serde_json::to_string(&speaker)?],
    )?;
    let mut affected = Vec::new();
    {
        let mut statement = tx.prepare(
            "SELECT document FROM transcripts WHERE EXISTS (
            SELECT 1 FROM json_each(transcripts.document,'$.speaker_turns')
            WHERE json_extract(value,'$.speaker_id')=?1)",
        )?;
        for row in statement.query_map([&id], |r| r.get::<_, String>(0))? {
            affected.push(serde_json::from_str::<crate::Transcript>(&row?)?);
        }
    }
    for mut entry in affected {
        resolve_names(&tx, &mut entry)?;
        tx.execute(
            "UPDATE transcripts SET document=?2 WHERE id=?1",
            params![entry.id, serde_json::to_string(&entry)?],
        )?;
        tx.execute(
            "UPDATE memory_fts SET text=?2 WHERE id=?1",
            params![entry.id, crate::history::search_text(&entry)],
        )?;
        let day = chrono::DateTime::parse_from_rfc3339(&entry.created_at)?
            .with_timezone(&chrono::Local)
            .date_naive()
            .to_string();
        tx.execute("INSERT INTO speaker_sync VALUES (?1,?2,?3,?4) ON CONFLICT(day,speaker_id) DO UPDATE SET new_name=excluded.new_name",params![day,id,old,name])?;
    }
    tx.commit()?;
    sync_names(paths, settings)
        .context("Speaker name saved; note synchronization will retry on the next app start")
}

/// Persisted outbox repairs interrupted name changes, including when daily jobs are off.
pub fn sync_names(paths: &Paths, settings: &Settings) -> Result<()> {
    let connection = connection(paths)?;
    let pending: Vec<(String, String, String, String)> = {
        let mut statement = connection
            .prepare("SELECT day,speaker_id,old_name,new_name FROM speaker_sync ORDER BY day")?;
        let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    for (day, id, old, new) in pending {
        crate::daily::sync_speaker_names(
            paths,
            settings,
            chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d")?,
            &old,
            &new,
        )?;
        connection.execute(
            "DELETE FROM speaker_sync WHERE day=?1 AND speaker_id=?2 AND old_name=?3 AND new_name=?4",
            params![day, id, old, new],
        )?;
    }
    Ok(())
}

fn normalized(values: &[f32]) -> Result<Vec<f32>> {
    let norm = values
        .iter()
        .map(|v| f64::from(*v).powi(2))
        .sum::<f64>()
        .sqrt();
    if values.len() < 2 || !values.iter().all(|v| v.is_finite()) || norm < 1e-6 {
        bail!("Invalid voice embedding");
    }
    Ok(values
        .iter()
        .map(|v| (f64::from(*v) / norm) as f32)
        .collect())
}

/// Register a new voice only when there is no plausible existing match. A close
/// competition remains Unknown rather than merging people or inventing profiles.
pub fn identify(
    paths: &Paths,
    embedding: &[f32],
    clip: &[f32],
    settings: &Settings,
) -> Result<Option<Speaker>> {
    let embedding = normalized(embedding)?;
    let mut connection = connection(paths)?;
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut candidates = Vec::new();
    {
        let mut statement = tx.prepare("SELECT document, embedding FROM speakers")?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (document, vector) = row?;
            let vector: Vec<f32> = serde_json::from_str(&vector)?;
            if vector.len() != embedding.len() {
                continue;
            }
            let vector = normalized(&vector)?;
            let score: f32 = vector.iter().zip(&embedding).map(|(a, b)| a * b).sum();
            candidates.push((score, serde_json::from_str::<Speaker>(&document)?));
        }
    }
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
    if let Some((score, speaker)) = candidates.first() {
        if *score >= settings.speaker_match_threshold {
            let runner_up = candidates.get(1).map(|v| v.0).unwrap_or(-1.0);
            if *score - runner_up < settings.speaker_match_margin {
                return Ok(None);
            }
            return Ok(Some(speaker.clone()));
        }
        // Borderline voices should not create duplicate identities.
        if *score >= settings.speaker_match_threshold - settings.speaker_match_margin {
            return Ok(None);
        }
    }
    if clip.len() < 32000 {
        return Ok(None);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let sequence: u64 = tx.query_row(
        "SELECT COALESCE(MAX(rowid), 0) + 1 FROM speakers",
        [],
        |r| r.get(0),
    )?;
    let directory = paths.root.join("speakers");
    std::fs::create_dir_all(&directory)?;
    let sample = directory.canonicalize()?.join(format!("{id}.wav"));
    let clip = &clip[..clip.len().min(240000)];
    let temporary = sample.with_extension("wav.part");
    audio::write_wav(&temporary, clip)?;
    std::fs::rename(&temporary, &sample)?;
    let speaker = Speaker {
        id,
        name: format!("Speaker {sequence}"),
        named: false,
        sample_path: sample.to_string_lossy().into_owned(),
        created_at: chrono::Utc::now().to_rfc3339(),
        sample_seconds: clip.len() as f64 / 16000.0,
    };
    if let Err(error) = tx.execute(
        "INSERT INTO speakers VALUES (?1,?2,0,?3,?4)",
        params![
            speaker.id,
            speaker.name,
            serde_json::to_string(&speaker)?,
            serde_json::to_string(&embedding)?
        ],
    ) {
        let _ = std::fs::remove_file(sample);
        return Err(error.into());
    }
    tx.commit()?;
    Ok(Some(speaker))
}

pub struct Engine {
    worker: Option<Worker>,
    last_used: std::time::Instant,
}
impl Default for Engine {
    fn default() -> Self {
        Self {
            worker: None,
            last_used: std::time::Instant::now(),
        }
    }
}
impl Engine {
    pub fn detect(
        &mut self,
        paths: &Paths,
        settings: &Settings,
        samples: &[f32],
    ) -> Result<Detection> {
        if self.worker.is_none() {
            self.worker = Some(Worker::start(paths, settings)?);
        }
        let input = paths
            .root
            .join("pending")
            .join(format!("{}.speaker.wav", uuid::Uuid::new_v4()));
        struct Temporary(PathBuf);
        impl Drop for Temporary {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _temporary = Temporary(input.clone());
        audio::write_wav(&input, samples)?;
        let response = match self
            .worker
            .as_mut()
            .unwrap()
            .request(serde_json::json!({"audio":input}))
        {
            Ok(response) => response,
            Err(error) => {
                self.worker = None;
                return Err(error);
            }
        };
        let detection: Detection = serde_json::from_value(response)?;
        let duration = samples.len() as f64 / 16000.0;
        if detection.turns.iter().any(|turn| {
            !turn.start.is_finite()
                || !turn.end.is_finite()
                || turn.start < 0.0
                || turn.end <= turn.start
                || turn.end > duration + 0.1
        }) {
            bail!("Invalid speaker turn timing; original transcript preserved");
        }
        self.last_used = std::time::Instant::now();
        Ok(detection)
    }
    pub fn unload_if_idle(&mut self, seconds: u64) -> bool {
        if seconds > 0 && self.worker.is_some() && self.last_used.elapsed().as_secs() >= seconds {
            self.unload();
            return true;
        }
        false
    }
    pub fn unload(&mut self) {
        self.worker = None;
    }
}
struct Worker {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    setup_lock: Option<std::fs::File>,
}
impl Worker {
    fn start(paths: &Paths, settings: &Settings) -> Result<Self> {
        let directory = paths.cache_dir.join("speaker-runtime");
        std::fs::create_dir_all(&directory)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join(".setup.lock"))?;
        lock.lock_exclusive()?;
        let python = if let Some(python) = &settings.speaker_python {
            python.clone()
        } else {
            let environment = directory.join("venv");
            let python = environment.join(if cfg!(windows) {
                "Scripts/python.exe"
            } else {
                "bin/python"
            });
            if !python.exists() {
                let output = Command::new("uv").args(["venv","--python","3.12"]).arg(&environment).output()
                    .context("Speaker detection needs uv or speaker_python pointing to Python with sherpa-onnx and numpy")?;
                if !output.status.success() {
                    bail!(
                        "Speaker runtime setup: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
            let marker = directory.join("sherpa-onnx-1.13.8-v2.ready");
            if !marker.exists() {
                let output = Command::new("uv")
                    .args(["pip", "install", "--python"])
                    .arg(&python)
                    .args(["sherpa-onnx==1.13.8", "numpy>=1.26,<3", "certifi>=2024.2"])
                    .output()?;
                if !output.status.success() {
                    bail!(
                        "Speaker dependencies: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                std::fs::write(marker, b"1.13.8\n")?;
            }
            python
        };
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(paths.logs_dir.join("speaker-worker.log"))?;
        let mut child = Command::new(python)
            .args(["-u", "-c", include_str!("speaker_worker.py")])
            .arg(directory.join("models"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()?;
        let input = child
            .stdin
            .take()
            .context("Speaker worker stdin unavailable")?;
        let output = BufReader::new(
            child
                .stdout
                .take()
                .context("Speaker worker stdout unavailable")?,
        );
        Ok(Self {
            child,
            input,
            output,
            setup_lock: Some(lock),
        })
    }
    fn request(&mut self, value: serde_json::Value) -> Result<serde_json::Value> {
        serde_json::to_writer(&mut self.input, &value)?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        let mut line = String::new();
        self.output.read_line(&mut line)?;
        self.setup_lock = None;
        let response: serde_json::Value = serde_json::from_str(&line)
            .context("Speaker worker exited or returned invalid JSON; see speaker-worker.log")?;
        if response["ok"] != true {
            bail!("Speaker detection: {}", response["error"]);
        }
        Ok(response)
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_new_ambiguous_short_and_invalid_voices_are_distinguished() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::in_directory(directory.path());
        paths.ensure_dirs().unwrap();
        let settings = Settings {
            speaker_match_threshold: 0.7,
            speaker_match_margin: 0.08,
            ..Settings::default()
        };
        assert!(!settings.retain_audio);
        let clip = vec![0.1; 16_000 * 20];
        let first = identify(&paths, &[1.0, 0.0, 0.0], &clip, &settings)
            .unwrap()
            .unwrap();
        let second = identify(&paths, &[0.0, 1.0, 0.0], &clip, &settings)
            .unwrap()
            .unwrap();
        assert_eq!(first.name, "Speaker 1");
        assert_eq!(second.name, "Speaker 2");
        assert_eq!(first.sample_seconds, 15.0);
        assert!(std::path::Path::new(&first.sample_path).exists());
        assert!(identify(&paths, &[0.707, 0.707, 0.0], &clip, &settings)
            .unwrap()
            .is_none());
        assert!(identify(&paths, &[0.65, 0.0, 0.76], &clip, &settings)
            .unwrap()
            .is_none());
        assert!(identify(&paths, &[0.0, 0.0, 1.0], &[0.1; 100], &settings)
            .unwrap()
            .is_none());
        assert!(identify(&paths, &[f32::NAN, 0.0], &clip, &settings).is_err());
        assert_eq!(list(&paths).unwrap().len(), 2);
        rename(&paths, &settings, &first.id, "Priya").unwrap();
        let future = identify(&paths, &[0.98, 0.02, 0.0], &clip, &settings)
            .unwrap()
            .unwrap();
        assert_eq!(future.id, first.id);
        assert_eq!(future.name, "Priya");
        assert_eq!(list(&paths).unwrap().len(), 2);
    }

    #[test]
    fn invalid_names_and_ids_cannot_change_profiles() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::in_directory(directory.path());
        paths.ensure_dirs().unwrap();
        let settings = Settings::default();
        let speaker = identify(&paths, &[1.0, 0.0], &vec![0.1; 32000], &settings)
            .unwrap()
            .unwrap();
        for name in ["", "\n", "Bad\nname"] {
            assert!(rename(&paths, &settings, &speaker.id, name).is_err());
        }
        assert!(rename(&paths, &settings, "../../settings.json", "Someone").is_err());
        assert!(rename(&paths, &settings, &speaker.id, &"a".repeat(81)).is_err());
        assert_eq!(list(&paths).unwrap()[0].name, speaker.name);
    }
}
