use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const PRODUCT_NAME: &str = "contextd";
pub const DATA_HOME_ENV: &str = "CONTEXTD_HOME";
const LEGACY_DATA_HOME_ENV: &str = "RECORD_EVERYTHING_HOME";

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortTranscriptPolicy {
    Keep,
    #[default]
    #[serde(alias = "skip_filler")]
    Cleanup,
    SkipAll,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CleanupBackend {
    #[default]
    Auto,
    Gguf,
    Mlx,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub microphone: Option<String>,
    pub microphones: Vec<String>,
    pub speech_model: String,
    pub filter_model: String,
    pub filter_enabled: bool,
    pub language: String,
    pub preferred_language: String,
    pub cleanup_backend: CleanupBackend,
    pub cleanup_source: Option<String>,
    pub mlx_python: Option<PathBuf>,
    pub data_directory: Option<PathBuf>,
    pub cache_directory: Option<PathBuf>,
    pub logs_directory: Option<PathBuf>,
    pub idle_unload_seconds: u64,
    pub silence_ms: u64,
    pub speech_threshold: f32,
    pub vad_threshold: f32,
    pub retain_audio: bool,
    pub short_transcript_policy: ShortTranscriptPolicy,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            microphone: None,
            microphones: Vec::new(),
            speech_model: "parakeet-unified-en-0.6b".into(),
            filter_model: crate::models::DEFAULT_FILTER_MODEL.into(),
            filter_enabled: false,
            language: "auto".into(),
            preferred_language: "auto".into(),
            cleanup_backend: CleanupBackend::Auto,
            cleanup_source: None,
            mlx_python: None,
            data_directory: None,
            cache_directory: None,
            logs_directory: None,
            idle_unload_seconds: 120,
            silence_ms: 1400,
            speech_threshold: 0.012,
            vad_threshold: 0.6,
            retain_audio: false,
            short_transcript_policy: ShortTranscriptPolicy::default(),
        }
    }
}

impl Settings {
    pub fn selected_microphones(&self) -> Vec<Option<String>> {
        if self.microphones.is_empty() {
            vec![self.microphone.clone()]
        } else {
            self.microphones.iter().cloned().map(Some).collect()
        }
    }
    pub fn uses_mlx(&self) -> bool {
        self.cleanup_backend == CleanupBackend::Mlx
            || self.cleanup_backend == CleanupBackend::Auto && self.filter_model.ends_with("-mlx")
    }
    pub fn speech_language(&self) -> &str {
        if self.language == "auto" {
            &self.preferred_language
        } else {
            &self.language
        }
    }
    /// UI presets; other Whisper ISO language codes remain available through JSON.
    pub fn cycle_language(&mut self) {
        self.language = match self.language.as_str() {
            "auto" => "en",
            "en" => "hi",
            "hi" => "hi-en",
            _ => "auto",
        }
        .into();
        if matches!(self.language.as_str(), "hi" | "hi-en") {
            if self.speech_model.starts_with("parakeet-")
                || matches!(
                    self.speech_model.as_str(),
                    "whisper-tiny" | "whisper-base" | "whisper-small"
                )
            {
                self.speech_model = "whisper-large-v3".into();
            }
            if crate::models::is_s1_model(&self.filter_model) {
                self.filter_model = "qwen3.5:0.8b".into();
            }
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.language.trim().is_empty() || self.preferred_language.trim().is_empty() {
            bail!("Language settings must be auto, hi-en, or a model-supported language code");
        }
        if let Some(source) = &self.cleanup_source {
            if source.trim().is_empty() {
                bail!("cleanup_source cannot be empty; use null to clear it");
            }
        } else {
            if self.cleanup_backend == CleanupBackend::Mlx && !self.filter_model.ends_with("-mlx") {
                bail!("Select an MLX cleanup model or provide an MLX cleanup_source");
            }
            if self.cleanup_backend == CleanupBackend::Gguf && self.filter_model.ends_with("-mlx") {
                bail!("Select a GGUF cleanup model for the GGUF backend");
            }
        }
        for directory in [
            &self.data_directory,
            &self.cache_directory,
            &self.logs_directory,
        ]
        .into_iter()
        .flatten()
        {
            if !directory.is_absolute() {
                bail!("Storage directories must be absolute paths");
            }
        }
        let mut inputs = std::collections::HashSet::new();
        if self
            .microphones
            .iter()
            .any(|id| id.is_empty() || !inputs.insert(id))
        {
            bail!("Microphone IDs must be nonempty and unique");
        }
        if self.speech_model.starts_with("parakeet-")
            && !matches!(self.speech_language(), "auto" | "en")
        {
            bail!("Parakeet supports English only. Select Whisper for Hindi + English.");
        }
        if !(300..=5000).contains(&self.silence_ms)
            || !self.speech_threshold.is_finite()
            || !(0.001..=0.2).contains(&self.speech_threshold)
            || !self.vad_threshold.is_finite()
            || !(0.1..=0.95).contains(&self.vad_threshold)
            || self.idle_unload_seconds > 86400
        {
            bail!("Invalid segmentation or idle-unload settings");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Paths {
    /// Durable notes, audio, pending jobs, history, and capture lock.
    #[serde(rename = "data")]
    pub root: PathBuf,
    #[serde(rename = "config")]
    pub config_dir: PathBuf,
    #[serde(rename = "cache")]
    pub cache_dir: PathBuf,
    #[serde(rename = "logs")]
    pub logs_dir: PathBuf,
}

impl Paths {
    /// Portable layout used by --data-dir and CONTEXTD_HOME.
    pub fn in_directory(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            config_dir: root.clone(),
            cache_dir: root.clone(),
            logs_dir: root.join("logs"),
            root,
        }
    }
    pub fn settings_path(&self) -> PathBuf {
        self.config_dir.join("settings.json")
    }
    pub fn models_dir(&self) -> PathBuf {
        self.cache_dir.join("models")
    }
    pub fn apply_storage(&mut self, settings: &Settings) -> Result<()> {
        settings.validate()?;
        if let Some(path) = &settings.data_directory {
            self.root = path.clone();
        }
        if let Some(path) = &settings.cache_directory {
            self.cache_dir = path.clone();
        }
        if let Some(path) = &settings.logs_directory {
            self.logs_dir = path.clone();
        }
        self.ensure_dirs()
    }
    pub fn discover() -> Result<Self> {
        if let Some(root) =
            std::env::var_os(DATA_HOME_ENV).or_else(|| std::env::var_os(LEGACY_DATA_HOME_ENV))
        {
            let paths = Self::in_directory(PathBuf::from(root));
            paths.ensure_dirs()?;
            return Ok(paths);
        }
        let home = directories::BaseDirs::new()
            .ok_or_else(|| anyhow::anyhow!("Cannot locate home directory"))?;
        #[cfg(unix)]
        let paths = {
            let xdg = |variable: &str, fallback: &str| {
                std::env::var_os(variable)
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .unwrap_or_else(|| home.home_dir().join(fallback))
                    .join(PRODUCT_NAME)
            };
            Self {
                root: xdg("XDG_DATA_HOME", ".local/share"),
                config_dir: xdg("XDG_CONFIG_HOME", ".config"),
                cache_dir: xdg("XDG_CACHE_HOME", ".cache"),
                logs_dir: xdg("XDG_STATE_HOME", ".local/state"),
            }
        };
        #[cfg(not(unix))]
        let paths = {
            let project = directories::ProjectDirs::from("", "", PRODUCT_NAME)
                .ok_or_else(|| anyhow::anyhow!("Cannot locate application directories"))?;
            Self {
                root: project.data_dir().to_path_buf(),
                config_dir: project.config_dir().to_path_buf(),
                cache_dir: project.cache_dir().to_path_buf(),
                logs_dir: project
                    .state_dir()
                    .unwrap_or_else(|| project.data_local_dir())
                    .join("logs"),
            }
        };
        let marker = paths.root.join(".storage-layout-v3");
        if !marker.exists() {
            #[cfg(target_os = "macos")]
            {
                // Replace only aliases made by our earlier single-directory migration.
                if std::fs::symlink_metadata(&paths.config_dir)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    let target = paths.config_dir.canonicalize()?;
                    for legacy in [
                        home.home_dir().join(".config/record_everything"),
                        home.home_dir().join("Library/Application Support/contextd"),
                    ] {
                        if legacy.canonicalize().is_ok_and(|path| path == target) {
                            std::fs::remove_file(&paths.config_dir)?;
                            std::fs::create_dir_all(&paths.config_dir)?;
                            break;
                        }
                    }
                }
                let legacy = home.home_dir().join("Library/Application Support/contextd");
                if legacy.is_dir() {
                    paths.migrate_legacy(&legacy)?;
                }
            }
            for legacy in [
                home.home_dir().join(".config/contextd"),
                home.home_dir().join(".config/record_everything"),
            ] {
                // A configuration-only XDG directory is already in its final location.
                if legacy == paths.config_dir
                    && ![
                        "models",
                        "logs",
                        "notes",
                        "audio",
                        "pending",
                        "history.sqlite3",
                    ]
                    .iter()
                    .any(|name| legacy.join(name).exists())
                {
                    continue;
                }
                if legacy.is_dir() {
                    paths.migrate_legacy(&legacy)?;
                }
            }
            paths.ensure_dirs()?;
            paths.relocate_audio_references()?;
            std::fs::write(&marker, b"3\n")?;
        }
        paths.ensure_dirs()?;
        Ok(paths)
    }
    pub fn ensure_dirs(&self) -> Result<()> {
        for directory in [
            self.config_dir.clone(),
            self.models_dir(),
            self.logs_dir.clone(),
            self.root.join("audio"),
            self.root.join("notes"),
            self.root.join("pending"),
        ] {
            std::fs::create_dir_all(directory)?;
        }
        Ok(())
    }
    fn relocate_audio_references(&self) -> Result<()> {
        let database = self.root.join("history.sqlite3");
        if database.exists() {
            let connection = rusqlite::Connection::open(database)?;
            connection.busy_timeout(std::time::Duration::from_secs(5))?;
            let audio = self
                .root
                .canonicalize()?
                .join("audio")
                .to_string_lossy()
                .into_owned();
            connection.execute(
                "UPDATE transcripts SET document = json_set(document, '$.audio_path', ?1 || '/' || id || '.wav')
                 WHERE json_type(document, '$.audio_path') = 'text'
                 AND json_extract(document, '$.audio_path') != ?1 || '/' || id || '.wav'",
                [audio],
            )?;
        }
        Ok(())
    }
    fn migrate_legacy(&self, legacy: &std::path::Path) -> Result<()> {
        use fs2::FileExt;
        let migration = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(legacy.join(".storage-migration.lock"))?;
        migration.lock_exclusive()?;
        let capture = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(legacy.join(".capture.lock"))?;
        capture.try_lock_exclusive().map_err(|_| {
            anyhow::anyhow!("Stop contextd recording before migrating storage directories")
        })?;
        #[cfg(target_os = "macos")]
        {
            let home = directories::BaseDirs::new()
                .ok_or_else(|| anyhow::anyhow!("Cannot locate home directory"))?;
            if legacy == home.home_dir().join("Library/Application Support/contextd") {
                migrate_path(
                    &home.home_dir().join("Library/Caches/contextd/models"),
                    &self.models_dir(),
                )?;
                migrate_path(
                    &home.home_dir().join("Library/Logs/contextd"),
                    &self.logs_dir,
                )?;
            }
        }
        for (source, destination) in [
            (legacy.join("settings.json"), self.settings_path()),
            (legacy.join("models"), self.models_dir()),
            (legacy.join("logs"), self.logs_dir.clone()),
            (legacy.join("notes"), self.root.join("notes")),
            (legacy.join("audio"), self.root.join("audio")),
            (legacy.join("pending"), self.root.join("pending")),
            (
                legacy.join("history.sqlite3"),
                self.root.join("history.sqlite3"),
            ),
            (
                legacy.join("history.sqlite3-wal"),
                self.root.join("history.sqlite3-wal"),
            ),
            (
                legacy.join("history.sqlite3-shm"),
                self.root.join("history.sqlite3-shm"),
            ),
            (
                legacy.join(".capture.lock"),
                self.root.join(".capture.lock"),
            ),
        ] {
            migrate_path(&source, &destination)?;
        }
        Ok(())
    }
    pub fn load(&self) -> Result<Settings> {
        let path = self.settings_path();
        let mut settings = if path.exists() {
            serde_json::from_slice(&std::fs::read(path)?)?
        } else {
            Settings::default()
        };
        let canonical = if settings.cleanup_source.is_some()
            || crate::models::cleanup_path(self, &settings.filter_model).is_ok()
        {
            crate::models::canonical_cleanup_id(&settings.filter_model)
        } else {
            // Migrate unsupported legacy Ollama names, not supported catalog
            // selections. Resetting all non-S1 IDs silently undid model choices.
            crate::models::DEFAULT_FILTER_MODEL
        }
        .to_owned();
        if settings.filter_model != canonical {
            settings.filter_model = canonical;
            self.save(&settings)?;
        }
        Ok(settings)
    }
    pub fn save(&self, settings: &Settings) -> Result<()> {
        settings.validate()?;
        let temp = self.config_dir.join("settings.json.part");
        std::fs::write(&temp, serde_json::to_vec_pretty(settings)?)?;
        std::fs::rename(temp, self.settings_path())?;
        Ok(())
    }
}

/// Move once, preserving legacy paths on Unix for existing audio references.
fn migrate_path(source: &std::path::Path, destination: &std::path::Path) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if source == destination || metadata.file_type().is_symlink() {
        return Ok(());
    }
    if destination.exists() {
        if source.canonicalize()? == destination.canonicalize()? {
            return Ok(());
        }
        bail!("Cannot migrate {}: destination {} already exists. Existing files were preserved; use --data-dir {} to access the legacy installation.", source.display(), destination.display(), source.parent().unwrap().display());
    }
    std::fs::create_dir_all(destination.parent().unwrap())?;
    if let Err(error) = std::fs::rename(source, destination) {
        if error.kind() != std::io::ErrorKind::CrossesDevices {
            return Err(error.into());
        }
        let temporary = destination.with_file_name(format!(
            ".{}-migration",
            destination.file_name().unwrap().to_string_lossy()
        ));
        copy_path(source, &temporary)?;
        std::fs::rename(&temporary, destination)?;
        if metadata.is_dir() {
            std::fs::remove_dir_all(source)?;
        } else {
            std::fs::remove_file(source)?;
        }
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(destination, source)?;
    Ok(())
}

fn copy_path(source: &std::path::Path, destination: &std::path::Path) -> Result<()> {
    if source.is_dir() {
        std::fs::create_dir_all(destination)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            if entry.file_type()?.is_symlink() {
                bail!(
                    "Cannot migrate a nested symlink automatically: {}",
                    entry.path().display()
                );
            }
            copy_path(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(source, destination)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_language_and_multiple_inputs_preserve_legacy_defaults() {
        let mut settings: Settings =
            serde_json::from_str(r#"{"microphone":"legacy","speech_model":"whisper-small"}"#)
                .unwrap();
        assert_eq!(settings.selected_microphones(), vec![Some("legacy".into())]);
        settings.microphones = vec!["first".into(), "second".into()];
        settings.preferred_language = "hi-en".into();
        assert_eq!(settings.speech_language(), "hi-en");
        assert_eq!(
            settings.selected_microphones(),
            vec![Some("first".into()), Some("second".into())]
        );
        settings.language = "en".into();
        assert_eq!(settings.speech_language(), "en");
        settings.microphones.push("first".into());
        assert!(settings.validate().is_err());
    }

    #[test]
    fn storage_overrides_keep_config_in_place_and_do_not_move_archives() {
        let directory = tempfile::tempdir().unwrap();
        let mut paths = Paths::in_directory(directory.path().join("original"));
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.root.join("notes/existing.md"), "preserved").unwrap();
        let settings = Settings {
            data_directory: Some(directory.path().join("data")),
            cache_directory: Some(directory.path().join("cache")),
            logs_directory: Some(directory.path().join("logs")),
            ..Settings::default()
        };
        paths.save(&settings).unwrap();
        let config_path = paths.settings_path();
        paths.apply_storage(&settings).unwrap();
        assert_eq!(paths.settings_path(), config_path);
        assert_eq!(paths.root, settings.data_directory.unwrap());
        assert!(paths.models_dir().is_dir());
        assert!(directory.path().join("original/notes/existing.md").exists());
    }

    #[test]
    fn supported_cleanup_selections_survive_restarts() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::in_directory(directory.path());
        paths.ensure_dirs().unwrap();
        for model in crate::models::catalog(&paths)
            .into_iter()
            .filter(|m| m.kind == "filter")
        {
            let settings = Settings {
                filter_model: model.id.clone(),
                filter_enabled: true,
                ..Settings::default()
            };
            paths.save(&settings).unwrap();
            assert_eq!(paths.load().unwrap().filter_model, model.id);
            assert_eq!(paths.load().unwrap().filter_model, model.id);
        }
        for alias in ["s1-mini", "s1-mini:latest", "old-ollama-model"] {
            paths
                .save(&Settings {
                    filter_model: alias.into(),
                    ..Settings::default()
                })
                .unwrap();
            assert_eq!(
                paths.load().unwrap().filter_model,
                crate::models::DEFAULT_FILTER_MODEL
            );
        }
    }
}
