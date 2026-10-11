use crate::Paths;
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// The daily Markdown destination, based on the local speech start date.
pub fn note_path(paths: &Paths, created_at: &str) -> Result<std::path::PathBuf> {
    let day = chrono::DateTime::parse_from_rfc3339(created_at)?
        .with_timezone(&chrono::Local)
        .date_naive();
    Ok(paths.root.join("notes").join(format!("{day}.md")))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transcript {
    pub id: String,
    pub created_at: String,
    pub raw: String,
    pub refined: String,
    pub speech_model: String,
    pub filter_model: Option<String>,
    pub duration_seconds: f64,
    pub audio_path: Option<String>,
    pub filter_error: Option<String>,
    #[serde(default)]
    pub microphone: Option<String>,
}

pub struct History(Connection, Paths);
impl History {
    pub fn open(paths: &Paths) -> Result<Self> {
        let conn = Connection::open(paths.root.join("history.sqlite3"))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS transcripts (
            id TEXT PRIMARY KEY, created_at TEXT NOT NULL, document TEXT NOT NULL);
            CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(id UNINDEXED, text, tokenize='unicode61');
            CREATE INDEX IF NOT EXISTS transcripts_time ON transcripts(julianday(created_at));")?;
        if conn.pragma_query_value(None, "user_version", |row| row.get::<_, i32>(0))? < 1 {
            conn.execute_batch(
                "INSERT INTO memory_fts(id, text)
                SELECT id, json_extract(document, '$.refined') FROM transcripts
                WHERE id NOT IN (SELECT id FROM memory_fts);
                PRAGMA user_version = 1;",
            )?;
        }
        Ok(Self(conn, paths.clone()))
    }
    pub fn insert(&self, transcript: &Transcript) -> Result<()> {
        let tx = self.0.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO transcripts VALUES (?1, ?2, ?3) ON CONFLICT(id) DO UPDATE SET document = excluded.document",
            params![
                transcript.id,
                transcript.created_at,
                serde_json::to_string(transcript)?
            ],
        )?;
        tx.execute("DELETE FROM memory_fts WHERE id = ?1", [&transcript.id])?;
        tx.execute(
            "INSERT INTO memory_fts(id, text) VALUES (?1, ?2)",
            params![transcript.id, transcript.refined],
        )?;
        tx.commit()?;
        self.sync_day(&transcript.created_at)?;
        Ok(())
    }
    pub fn list(&self, limit: u32) -> Result<Vec<Transcript>> {
        let mut stmt = self
            .0
            .prepare("SELECT document FROM transcripts ORDER BY created_at DESC LIMIT ?1")?;
        let rows = stmt.query_map([limit.min(10000)], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
    pub fn all(&self) -> Result<Vec<Transcript>> {
        let mut statement = self
            .0
            .prepare("SELECT document FROM transcripts ORDER BY created_at DESC")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
    pub fn delete(&self, id: &str) -> Result<()> {
        let created_at: Option<String> = self
            .0
            .query_row(
                "SELECT created_at FROM transcripts WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let tx = self.0.unchecked_transaction()?;
        tx.execute("DELETE FROM memory_fts WHERE id = ?1", [id])?;
        tx.execute("DELETE FROM transcripts WHERE id = ?1", [id])?;
        tx.commit()?;
        if let Some(time) = created_at {
            self.sync_day(&time)?;
        }
        Ok(())
    }
    pub fn update(&self, transcript: &Transcript) -> Result<()> {
        let tx = self.0.unchecked_transaction()?;
        tx.execute(
            "UPDATE transcripts SET document = ?2 WHERE id = ?1",
            params![transcript.id, serde_json::to_string(transcript)?],
        )?;
        tx.execute(
            "UPDATE memory_fts SET text = ?2 WHERE id = ?1",
            params![transcript.id, transcript.refined],
        )?;
        tx.commit()?;
        self.sync_day(&transcript.created_at)
    }
    /// Atomic Markdown replacement under a cross-process lock.
    pub fn sync_day(&self, created_at: &str) -> Result<()> {
        use fs2::FileExt;
        let day = chrono::DateTime::parse_from_rfc3339(created_at)?
            .with_timezone(&chrono::Local)
            .date_naive();
        let directory = self.1.root.join("notes");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join(".write.lock"))?;
        lock.lock_exclusive()?;
        // A UTC window covering every local timezone bounds the scan to three
        // days. Filter exact local dates below; avoid rereading years of memory.
        let lower = day.and_hms_opt(0, 0, 0).unwrap().and_utc() - chrono::Duration::days(1);
        let upper = lower + chrono::Duration::days(3);
        let mut statement = self.0.prepare(
            "SELECT document FROM transcripts
            WHERE julianday(created_at) >= julianday(?1) AND julianday(created_at) < julianday(?2)
            ORDER BY julianday(created_at) ASC",
        )?;
        let rows = statement.query_map(params![lower.to_rfc3339(), upper.to_rfc3339()], |r| {
            r.get::<_, String>(0)
        })?;
        let mut text = format!(
            "# {}\n\nAll times are local. Headings indicate approximate speech start times.\n\n",
            day
        );
        for row in rows {
            let entry: Transcript = serde_json::from_str(&row?)?;
            let time = chrono::DateTime::parse_from_rfc3339(&entry.created_at)?
                .with_timezone(&chrono::Local);
            if time.date_naive() != day {
                continue;
            }
            text.push_str(&format!(
                "## Around {}\n\n<!-- memory-id: {}; started-at: {} -->\n\n{}\n\n",
                time.format("%-I:%M %p"),
                entry.id,
                entry.created_at,
                entry.refined
            ));
            if let Some(microphone) = &entry.microphone {
                let microphone = microphone.replace(['\r', '\n'], " ").replace('`', "\\`");
                text.push_str(&format!("Source microphone: {microphone}\n\n"));
            }
            if entry.refined != entry.raw {
                text.push_str(&format!(
                    "<details>\n<summary>Original transcript</summary>\n\n{}\n\n</details>\n\n",
                    entry.raw
                ));
            }
        }
        let temporary = directory.join(format!("{day}.md.part"));
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, note_path(&self.1, created_at)?)?;
        lock.unlock()?;
        Ok(())
    }
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<Transcript>> {
        let terms: Vec<_> = query
            .split_whitespace()
            .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
            .collect();
        if terms.is_empty() {
            return self.list(limit);
        }
        let mut stmt = self.0.prepare(
            "SELECT t.document FROM memory_fts f JOIN transcripts t ON t.id = f.id
            WHERE memory_fts MATCH ?1 ORDER BY bm25(memory_fts), t.created_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![terms.join(" OR "), limit.min(1000)], |r| {
            r.get::<_, String>(0)
        })?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn daily_markdown_tracks_raw_cleanup_search_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_directory(dir.path().to_owned());
        std::fs::create_dir(paths.root.join("notes")).unwrap();
        let history = History::open(&paths).unwrap();
        let local = chrono::Local::now();
        let mut entry = Transcript {
            id: "test-memory".into(),
            created_at: local.to_rfc3339(),
            raw: "Um ship Tuesday no Wednesday".into(),
            refined: "Um ship Tuesday no Wednesday".into(),
            speech_model: "parakeet-unified-en-0.6b".into(),
            filter_model: None,
            duration_seconds: 5.0,
            audio_path: None,
            filter_error: None,
            microphone: None,
        };
        history.insert(&entry).unwrap();
        // Replaying a completed durable job must not duplicate the note or FTS row.
        history.insert(&entry).unwrap();
        assert_eq!(history.list(10).unwrap().len(), 1);
        let file = paths
            .root
            .join("notes")
            .join(format!("{}.md", local.date_naive()));
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains(&format!("## Around {}", local.format("%-I:%M %p"))));
        assert!(text.contains(&entry.raw));
        entry.refined = "Ship Wednesday".into();
        history.update(&entry).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("Ship Wednesday"));
        assert!(text.contains("Original transcript"));
        assert_eq!(history.search("Wednesday", 10).unwrap().len(), 1);
        assert!(history.search("Tuesday", 10).unwrap().is_empty());
        assert!(history.search("\" OR *", 10).is_ok());
        history.delete(&entry.id).unwrap();
        assert!(!std::fs::read_to_string(file)
            .unwrap()
            .contains("Ship Wednesday"));
        assert!(history.search("Wednesday", 10).unwrap().is_empty());
    }

    #[test]
    fn notes_are_split_by_local_start_date_not_processing_date() {
        use chrono::TimeZone;
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::in_directory(directory.path().to_owned());
        std::fs::create_dir(paths.root.join("notes")).unwrap();
        let history = History::open(&paths).unwrap();
        for (id, day, hour, text) in [
            ("first", 7, 14, "First afternoon thought"),
            ("second", 8, 0, "After midnight thought"),
        ] {
            let start = chrono::Local
                .with_ymd_and_hms(2026, 10, day, hour, 0, 0)
                .unwrap();
            history
                .insert(&Transcript {
                    id: id.into(),
                    created_at: start.with_timezone(&chrono::Utc).to_rfc3339(),
                    raw: text.into(),
                    refined: text.into(),
                    speech_model: "manual-note".into(),
                    filter_model: None,
                    duration_seconds: 0.0,
                    audio_path: None,
                    filter_error: None,
                    microphone: None,
                })
                .unwrap();
        }
        let first = std::fs::read_to_string(paths.root.join("notes/2026-10-07.md")).unwrap();
        let second = std::fs::read_to_string(paths.root.join("notes/2026-10-08.md")).unwrap();
        assert!(first.contains("## Around 2:00 PM"));
        assert!(first.contains("First afternoon thought"));
        assert!(!first.contains("After midnight thought"));
        assert!(second.contains("## Around 12:00 AM"));
        assert!(second.contains("After midnight thought"));
        assert!(!second.contains("First afternoon thought"));
    }
}
