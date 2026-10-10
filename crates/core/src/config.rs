use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const PRODUCT_NAME: &str = "contextd";
pub const DATA_HOME_ENV: &str = "CONTEXTD_HOME";
const LEGACY_DATA_HOME_ENV: &str = "RECORD_EVERYTHING_HOME";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub microphone: Option<String>,
    pub speech_model: String,
    pub filter_model: String,
    pub filter_enabled: bool,
    pub language: String,
    pub idle_unload_seconds: u64,
    pub silence_ms: u64,
    pub max_segment_seconds: u64,
    pub speech_threshold: f32,
    pub retain_audio: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            microphone: None,
            speech_model: "parakeet-unified-en-0.6b".into(),
            filter_model: crate::models::DEFAULT_FILTER_MODEL.into(),
            filter_enabled: false,
            language: "auto".into(),
            idle_unload_seconds: 120,
            silence_ms: 1400,
            max_segment_seconds: 30,
            speech_threshold: 0.012,
            retain_audio: false,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        if !(300..=5000).contains(&self.silence_ms)
            || !(5..=60).contains(&self.max_segment_seconds)
            || !self.speech_threshold.is_finite()
            || !(0.001..=0.2).contains(&self.speech_threshold)
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
        if settings.filter_model != crate::models::DEFAULT_FILTER_MODEL {
            // Older releases stored Ollama model names here. The embedded
            // runtime currently supports S1-mini only, so move that preference
            // to the supported catalog model and persist the migration.
            settings.filter_model = crate::models::DEFAULT_FILTER_MODEL.into();
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
