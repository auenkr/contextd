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
        CREATE TABLE IF NOT EXISTS dismissed_speaker_turns (id TEXT PRIMARY KEY);
        CREATE TABLE IF NOT EXISTS speaker_aliases (id TEXT PRIMARY KEY, speaker_id TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS speaker_embeddings (id TEXT PRIMARY KEY, speaker_id TEXT NOT NULL, embedding TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS speaker_training_examples (
            id TEXT PRIMARY KEY, speaker_id TEXT, embedding TEXT NOT NULL,
            created_at TEXT NOT NULL, trained INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS speaker_prototypes (
            speaker_id TEXT PRIMARY KEY, embedding TEXT NOT NULL, example_count INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS speaker_learning_state (
            id INTEGER PRIMARY KEY CHECK (id = 1), last_trained_at TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS speaker_sync (day TEXT NOT NULL, speaker_id TEXT NOT NULL, old_name TEXT NOT NULL, new_name TEXT NOT NULL, PRIMARY KEY(day,speaker_id));",
    )?;
    // Backfill user-confirmed links created before adaptive profiles existed.
    // Skip embeddings already present in the labeled training table.
    connection.execute(
        "INSERT OR IGNORE INTO speaker_training_examples (id,speaker_id,embedding,created_at,trained)
         SELECT 'legacy:' || e.id, e.speaker_id, e.embedding,
                strftime('%Y-%m-%dT%H:%M:%f+00:00','now'), 0
         FROM speaker_embeddings e
         WHERE NOT EXISTS (
             SELECT 1 FROM speaker_training_examples t
             WHERE t.speaker_id=e.speaker_id AND t.embedding=e.embedding
         )",
        [],
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
            let canonical = connection
                .query_row(
                    "SELECT speaker_id FROM speaker_aliases WHERE id=?1",
                    [id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            if let Some(canonical) = canonical {
                turn.speaker_id = Some(canonical);
            }
            let id = turn.speaker_id.as_ref().unwrap();
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

/// Link a duplicate profile, or `unknown:TRANSCRIPT_ID:TURN_INDEX` (zero based),
/// to a saved identity. Keep every confirmed embedding instead of averaging voices.
pub fn link(paths: &Paths, settings: &Settings, source: &str, target: &str) -> Result<()> {
    use rusqlite::OptionalExtension;
    crate::history::History::open(paths)?;
    let target = uuid::Uuid::parse_str(target)?.to_string();
    let mut connection = connection(paths)?;
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let name: String = tx
        .query_row("SELECT name FROM speakers WHERE id=?1", [&target], |r| {
            r.get(0)
        })
        .context("Target speaker not found")?;
    let unknown = source
        .strip_prefix("unknown:")
        .map(|value| -> Result<(String, usize)> {
            let (id, index) = value
                .rsplit_once(':')
                .context("Use unknown:TRANSCRIPT_ID:TURN_INDEX")?;
            Ok((uuid::Uuid::parse_str(id)?.to_string(), index.parse()?))
        })
        .transpose()?;
    let old = if let Some((transcript_id, index)) = &unknown {
        let example_id = format!("unknown:{transcript_id}:{index}");
        let embedding: Option<String> = tx
            .query_row(
                "SELECT embedding FROM speaker_training_examples WHERE id=?1",
                [&example_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(embedding) = embedding {
            tx.execute(
                "INSERT OR REPLACE INTO speaker_embeddings VALUES (?1,?2,?3)",
                params![example_id, target, embedding],
            )?;
            tx.execute(
                "UPDATE speaker_training_examples SET speaker_id=?2, trained=0 WHERE id=?1",
                params![example_id, target],
            )?;
        }
        "Unknown speaker".to_string()
    } else {
        let source = uuid::Uuid::parse_str(source)?.to_string();
        if source == target {
            bail!("Choose a different speaker to link to");
        }
        let old: String = tx
            .query_row("SELECT name FROM speakers WHERE id=?1", [&source], |r| {
                r.get(0)
            })
            .context("Source speaker not found")?;
        let mut examples = vec![(
            format!("linked:{source}:{source}"),
            tx.query_row(
                "SELECT embedding FROM speakers WHERE id=?1",
                [&source],
                |row| row.get::<_, String>(0),
            )?,
        )];
        {
            let mut statement =
                tx.prepare("SELECT id, embedding FROM speaker_embeddings WHERE speaker_id=?1")?;
            examples.extend(
                statement
                    .query_map([&source], |row| {
                        Ok((
                            format!("linked:{source}:{}", row.get::<_, String>(0)?),
                            row.get::<_, String>(1)?,
                        ))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?,
            );
        }
        for (id, embedding) in examples {
            tx.execute(
                "INSERT OR IGNORE INTO speaker_training_examples (id,speaker_id,embedding,created_at,trained) VALUES (?1,?2,?3,?4,0)",
                params![id, target, embedding, chrono::Utc::now().to_rfc3339()],
            )?;
        }
        tx.execute(
            "INSERT INTO speaker_embeddings SELECT id,?2,embedding FROM speakers WHERE id=?1",
            params![source, target],
        )?;
        tx.execute(
            "UPDATE speaker_embeddings SET speaker_id=?2 WHERE speaker_id=?1",
            params![source, target],
        )?;
        tx.execute(
            "UPDATE speaker_aliases SET speaker_id=?2 WHERE speaker_id=?1",
            params![source, target],
        )?;
        tx.execute(
            "INSERT INTO speaker_aliases VALUES (?1,?2)",
            params![source, target],
        )?;
        tx.execute(
            "UPDATE speaker_training_examples SET speaker_id=?2, trained=0 WHERE speaker_id=?1",
            params![source, target],
        )?;
        tx.execute(
            "DELETE FROM speaker_prototypes WHERE speaker_id=?1",
            [&source],
        )?;
        tx.execute("DELETE FROM speakers WHERE id=?1", [&source])?;
        old
    };
    let documents: Vec<String> = {
        let (query, key) = if let Some((id, _)) = &unknown {
            ("SELECT document FROM transcripts WHERE id=?1", id.as_str())
        } else {
            (
                "SELECT document FROM transcripts WHERE EXISTS (
                 SELECT 1 FROM json_each(transcripts.document,'$.speaker_turns')
                 WHERE json_extract(value,'$.speaker_id')=?1)",
                source,
            )
        };
        let mut statement = tx.prepare(query)?;
        let rows = statement.query_map([key], |r| r.get(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let mut changed = false;
    for document in documents {
        let mut entry: crate::Transcript = serde_json::from_str(&document)?;
        if let Some((id, index)) = &unknown {
            if &entry.id != id {
                continue;
            }
            let turn = entry
                .speaker_turns
                .get_mut(*index)
                .context("Speaker turn not found")?;
            if turn.speaker_id.is_some() {
                bail!("This turn already has a speaker; link its profile instead");
            }
            turn.speaker_id = Some(target.clone());
        } else if !entry
            .speaker_turns
            .iter()
            .any(|t| t.speaker_id.as_deref() == Some(source))
        {
            continue;
        }
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
        // Unknown is a per-turn correction: never replace every unknown in a title.
        let title_old = if unknown.is_some() { &name } else { &old };
        tx.execute("INSERT INTO speaker_sync VALUES (?1,?2,?3,?4) ON CONFLICT(day,speaker_id) DO UPDATE SET new_name=excluded.new_name", params![day,source,title_old,name])?;
        changed = true;
    }
    if unknown.is_some() && !changed {
        bail!("Transcript not found");
    }
    tx.commit()?;
    sync_names(paths, settings)
        .context("Link saved; note synchronization will retry on next app start")
}

/// Keep embeddings for unassigned, non-overlapping turns until a person confirms
/// their identity. The audio itself remains governed by normal retention rules.
pub fn store_unlabeled_examples(
    paths: &Paths,
    transcript_id: &str,
    turns: &[SpeakerTurn],
    embeddings: &[Option<Vec<f32>>],
) -> Result<()> {
    let connection = connection(paths)?;
    for (index, (turn, embedding)) in turns.iter().zip(embeddings).enumerate() {
        if turn.speaker_id.is_some() || turn.overlap {
            continue;
        }
        let Some(embedding) = embedding else { continue };
        let embedding = normalized(embedding)?;
        let id = format!("unknown:{transcript_id}:{index}");
        connection.execute(
            "INSERT OR IGNORE INTO speaker_training_examples (id,speaker_id,embedding,created_at,trained) VALUES (?1,NULL,?2,?3,0)",
            params![id, serde_json::to_string(&embedding)?, chrono::Utc::now().to_rfc3339()],
        )?;
    }
    Ok(())
}

/// Train and persist the personalized speaker-matching weights after enough
/// new, explicitly labeled examples have accumulated and the cooldown elapsed.
/// The downloaded WeSpeaker embedding network stays frozen; these class
/// centroids are the small supervised matching head used by future scans.
pub fn train_profiles_if_due(paths: &Paths) -> Result<bool> {
    use rusqlite::OptionalExtension;
    const MIN_NEW_EXAMPLES: i64 = 10;
    const COOLDOWN: chrono::Duration = chrono::Duration::hours(6);
    let mut connection = connection(paths)?;
    let (new_examples, oldest_new): (i64, Option<String>) = connection.query_row(
        "SELECT COUNT(*), MIN(created_at) FROM speaker_training_examples WHERE speaker_id IS NOT NULL AND trained=0",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if new_examples < MIN_NEW_EXAMPLES {
        return Ok(false);
    }
    let now = chrono::Utc::now();
    let last_trained: Option<String> = connection
        .query_row(
            "SELECT last_trained_at FROM speaker_learning_state WHERE id=1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let cooldown_start = last_trained
        .or(oldest_new)
        .context("No speaker examples to train")?;
    let cooldown_start =
        chrono::DateTime::parse_from_rfc3339(&cooldown_start)?.with_timezone(&chrono::Utc);
    if now - cooldown_start < COOLDOWN {
        return Ok(false);
    }

    let tx = connection.transaction()?;
    let mut grouped = std::collections::HashMap::<String, Vec<Vec<f32>>>::new();
    {
        let mut statement = tx.prepare(
            "SELECT speaker_id, embedding FROM speaker_training_examples WHERE speaker_id IS NOT NULL",
        )?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (speaker_id, embedding) = row?;
            grouped
                .entry(speaker_id)
                .or_default()
                .push(serde_json::from_str(&embedding)?);
        }
    }
    let mut trained = 0usize;
    for (speaker_id, examples) in grouped {
        let dimensions = examples.first().map_or(0, Vec::len);
        let mut prototype = vec![0.0f32; dimensions];
        let mut usable = 0usize;
        for example in examples {
            if example.len() != dimensions || !example.iter().all(|value| value.is_finite()) {
                continue;
            }
            for (sum, value) in prototype.iter_mut().zip(example) {
                *sum += value;
            }
            usable += 1;
        }
        if usable == 0 {
            continue;
        }
        for value in &mut prototype {
            *value /= usable as f32;
        }
        let prototype = normalized(&prototype)?;
        tx.execute(
            "INSERT INTO speaker_prototypes VALUES (?1,?2,?3) ON CONFLICT(speaker_id) DO UPDATE SET embedding=excluded.embedding, example_count=excluded.example_count",
            params![speaker_id, serde_json::to_string(&prototype)?, usable as i64],
        )?;
        trained += 1;
    }
    if trained == 0 {
        return Ok(false);
    }
    tx.execute(
        "UPDATE speaker_training_examples SET trained=1 WHERE speaker_id IS NOT NULL",
        [],
    )?;
    tx.execute(
        "INSERT INTO speaker_learning_state (id,last_trained_at) VALUES (1,?1) ON CONFLICT(id) DO UPDATE SET last_trained_at=excluded.last_trained_at",
        [now.to_rfc3339()],
    )?;
    tx.commit()?;
    Ok(true)
}

/// Forget enrolled voices, or dismiss unknown turns from the browser. History is preserved.
pub fn delete(paths: &Paths, ids: &[String]) -> Result<()> {
    let mut connection = connection(paths)?;
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for id in ids {
        if let Some(value) = id.strip_prefix("unknown:") {
            let (transcript, index) = value.rsplit_once(':').context("Invalid unknown turn ID")?;
            uuid::Uuid::parse_str(transcript)?;
            let _: usize = index.parse()?;
            tx.execute(
                "INSERT OR IGNORE INTO dismissed_speaker_turns VALUES (?1)",
                [id],
            )?;
            tx.execute("DELETE FROM speaker_training_examples WHERE id=?1", [id])?;
        } else {
            let id = uuid::Uuid::parse_str(id)?.to_string();
            tx.execute("DELETE FROM speaker_embeddings WHERE speaker_id=?1", [&id])?;
            tx.execute(
                "DELETE FROM speaker_training_examples WHERE speaker_id=?1",
                [&id],
            )?;
            tx.execute("DELETE FROM speaker_prototypes WHERE speaker_id=?1", [&id])?;
            tx.execute("DELETE FROM speaker_aliases WHERE speaker_id=?1", [&id])?;
            if tx.execute("DELETE FROM speakers WHERE id=?1", [&id])? == 0 {
                bail!("Speaker not found");
            }
        }
    }
    tx.commit()?;
    Ok(())
}

/// Unknown turns can be selected and linked independently in the speaker browser.
pub fn unknown_turns(paths: &Paths) -> Result<Vec<Speaker>> {
    let history = crate::history::History::open(paths)?;
    let connection = connection(paths)?;
    let dismissed: std::collections::HashSet<String> = {
        let mut statement = connection.prepare("SELECT id FROM dismissed_speaker_turns")?;
        let rows = statement.query_map([], |r| r.get(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let mut result = Vec::new();
    for entry in history.list(10000)? {
        for (index, turn) in entry.speaker_turns.iter().enumerate() {
            if turn.speaker_id.is_none()
                && !dismissed.contains(&format!("unknown:{}:{}", entry.id, index))
            {
                result.push(Speaker {
                    id: format!("unknown:{}:{}", entry.id, index),
                    name: format!(
                        "Unknown · {} · {:.1}s",
                        entry.created_at, turn.start_seconds
                    ),
                    named: false,
                    sample_path: entry.audio_path.clone().unwrap_or_default(),
                    created_at: entry.created_at.clone(),
                    sample_seconds: turn.end_seconds - turn.start_seconds,
                });
            }
        }
    }
    Ok(result)
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
/// The async-trained personalized head is an additional, stricter candidate:
/// only a strong centroid match can affect attribution.
pub fn identify(
    paths: &Paths,
    embedding: &[f32],
    clip: &[f32],
    settings: &Settings,
) -> Result<Option<Speaker>> {
    let embedding = normalized(embedding)?;
    let mut connection = connection(paths)?;
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut candidates: Vec<(f32, Speaker)> = Vec::new();
    {
        let mut statement = tx.prepare(
            "SELECT s.document, e.embedding FROM speakers s JOIN (
            SELECT id AS speaker_id, embedding FROM speakers
            UNION ALL SELECT speaker_id, embedding FROM speaker_embeddings
        ) e ON s.id=e.speaker_id",
        )?;
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
            let speaker = serde_json::from_str::<Speaker>(&document)?;
            if let Some((best, _)) = candidates
                .iter_mut()
                .find(|(_, existing)| existing.id == speaker.id)
            {
                *best = best.max(score);
            } else {
                candidates.push((score, speaker));
            }
        }
    }
    // These persisted class weights are rebuilt by contextd's background
    // learner. Use a stricter floor than the exemplar matcher: a prototype
    // averages confirmed variants and must be especially clear before it can
    // claim an identity. The normal runner-up margin still applies below.
    {
        let model_threshold = settings.speaker_match_threshold.max(0.82);
        let mut statement = tx.prepare(
            "SELECT s.document, p.embedding FROM speaker_prototypes p
             JOIN speakers s ON s.id=p.speaker_id",
        )?;
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
            if score < model_threshold {
                continue;
            }
            let speaker = serde_json::from_str::<Speaker>(&document)?;
            if let Some((best, _)) = candidates
                .iter_mut()
                .find(|(_, existing)| existing.id == speaker.id)
            {
                *best = best.max(score);
            } else {
                candidates.push((score, speaker));
            }
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
    fn deleting_voice_removes_linked_matching_vectors() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::in_directory(directory.path());
        paths.ensure_dirs().unwrap();
        let settings = Settings::default();
        let clip = vec![0.1; 32000];
        let first = identify(&paths, &[1.0, 0.0], &clip, &settings)
            .unwrap()
            .unwrap();
        let second = identify(&paths, &[0.0, 1.0], &clip, &settings)
            .unwrap()
            .unwrap();
        link(&paths, &settings, &second.id, &first.id).unwrap();
        delete(&paths, &[first.id.clone()]).unwrap();
        assert!(list(&paths).unwrap().is_empty());
        let replacement = identify(&paths, &[0.0, 1.0], &clip, &settings)
            .unwrap()
            .unwrap();
        assert_ne!(replacement.id, first.id);
    }

    #[test]
    fn linking_preserves_voice_variants_and_corrects_only_selected_unknown() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::in_directory(directory.path());
        paths.ensure_dirs().unwrap();
        let settings = Settings {
            speaker_match_threshold: 0.9,
            ..Settings::default()
        };
        let clip = vec![0.1; 32000];
        let first = identify(&paths, &[1.0, 0.0], &clip, &settings)
            .unwrap()
            .unwrap();
        let second = identify(&paths, &[0.0, 1.0], &clip, &settings)
            .unwrap()
            .unwrap();
        let history = crate::history::History::open(&paths).unwrap();
        let entry = crate::Transcript {
            id: uuid::Uuid::new_v4().to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            raw: "Hello".into(),
            refined: "Hello".into(),
            speech_model: "test".into(),
            filter_model: None,
            duration_seconds: 3.0,
            audio_path: None,
            filter_error: None,
            microphone: None,
            speaker_error: None,
            speaker_turns: vec![Some(second.id.clone()), None, None]
                .into_iter()
                .map(|id| SpeakerTurn {
                    speaker_id: id,
                    speaker_name: "Unknown speaker".into(),
                    start_seconds: 0.0,
                    end_seconds: 1.0,
                    raw: "Hello".into(),
                    refined: "Hello".into(),
                    overlap: false,
                })
                .collect(),
        };
        history.insert(&entry).unwrap();
        // A daily inference job must not make linking wait for its file lock.
        // The identity commits first, and the durable outbox retries the files.
        std::fs::create_dir_all(paths.root.join("structured")).unwrap();
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(paths.root.join("structured/.write.lock"))
            .unwrap();
        lock.lock_exclusive().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker_paths = paths.clone();
        let worker_settings = settings.clone();
        let source = second.id.clone();
        let target = first.id.clone();
        let worker = std::thread::spawn(move || {
            sender
                .send(link(&worker_paths, &worker_settings, &source, &target))
                .unwrap();
        });
        let result = receiver.recv_timeout(std::time::Duration::from_secs(2));
        lock.unlock().unwrap();
        worker.join().unwrap();
        assert!(result
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("Link saved"));
        let queued: usize = connection(&paths)
            .unwrap()
            .query_row("SELECT count(*) FROM speaker_sync", [], |row| row.get(0))
            .unwrap();
        assert_eq!(queued, 1);
        sync_names(&paths, &settings).unwrap();
        let queued: usize = connection(&paths)
            .unwrap()
            .query_row("SELECT count(*) FROM speaker_sync", [], |row| row.get(0))
            .unwrap();
        assert_eq!(queued, 0);
        // Source-note contention must also leave the outbox retryable.
        let note_lock = std::fs::OpenOptions::new()
            .write(true)
            .open(paths.root.join("notes/.write.lock"))
            .unwrap();
        note_lock.lock_exclusive().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker_paths = paths.clone();
        let worker_settings = settings.clone();
        let target = first.id.clone();
        let worker = std::thread::spawn(move || {
            sender
                .send(rename(&worker_paths, &worker_settings, &target, "Golden"))
                .unwrap();
        });
        let result = receiver.recv_timeout(std::time::Duration::from_secs(2));
        note_lock.unlock().unwrap();
        worker.join().unwrap();
        assert!(result
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("Speaker name saved"));
        sync_names(&paths, &settings).unwrap();
        assert_eq!(list(&paths).unwrap().len(), 1);
        assert_eq!(
            identify(&paths, &[0.0, 1.0], &clip, &settings)
                .unwrap()
                .unwrap()
                .id,
            first.id
        );
        link(
            &paths,
            &settings,
            &format!("unknown:{}:1", entry.id),
            &first.id,
        )
        .unwrap();
        let updated = history.list(1).unwrap().remove(0);
        assert_eq!(
            updated.speaker_turns[0].speaker_id.as_ref(),
            Some(&first.id)
        );
        assert_eq!(
            updated.speaker_turns[1].speaker_id.as_ref(),
            Some(&first.id)
        );
        assert!(updated.speaker_turns[2].speaker_id.is_none());
        let unknown_id = format!("unknown:{}:2", entry.id);
        delete(&paths, &[unknown_id.clone()]).unwrap();
        assert!(!unknown_turns(&paths)
            .unwrap()
            .iter()
            .any(|s| s.id == unknown_id));
        assert!(history.list(10).unwrap()[0].speaker_turns[2]
            .speaker_id
            .is_none());
        assert!(link(&paths, &settings, &first.id, &first.id).is_err());
        // An in-flight recording using the old identity resolves to its canonical speaker.
        let mut stale = entry.clone();
        resolve_names(&connection(&paths).unwrap(), &mut stale).unwrap();
        assert_eq!(stale.speaker_turns[0].speaker_id.as_ref(), Some(&first.id));
        rename(&paths, &settings, &first.id, "Golden").unwrap();
        assert_eq!(
            history.list(1).unwrap()[0].speaker_turns[1].speaker_name,
            "Golden"
        );
    }

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
