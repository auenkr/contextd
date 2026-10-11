use crate::{Event, Paths};
use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::{io::AsyncWriteExt, sync::broadcast};

pub const DEFAULT_FILTER_MODEL: &str = "hf.co/superwhisper/s1-mini-GGUF:Q4_K_M";
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranslationSupport {
    Supported,
    Unsupported,
}

pub fn cleanup_translation_support(id: &str) -> Option<TranslationSupport> {
    cleanup_model(id)
        .ok()
        .map(|model| model.translation_support)
}

pub fn translation_warning(settings: &crate::Settings) -> Option<String> {
    if settings.cleanup_source.is_none()
        && cleanup_translation_support(&settings.filter_model)
            == Some(TranslationSupport::Unsupported)
    {
        let name = cleanup_model(&settings.filter_model).ok()?.name;
        Some(format!("{name} does not support translation, so translation will not work. Select a multilingual cleanup model."))
    } else {
        None
    }
}
/// Common output targets. Other model-supported languages can be entered by name.
pub const OUTPUT_LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("hi", "Hindi"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("zh", "Chinese"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("ru", "Russian"),
    ("ar", "Arabic"),
    ("bn", "Bengali"),
    ("ta", "Tamil"),
    ("te", "Telugu"),
    ("mr", "Marathi"),
    ("gu", "Gujarati"),
    ("kn", "Kannada"),
    ("ml", "Malayalam"),
    ("pa", "Punjabi"),
    ("ur", "Urdu"),
    ("th", "Thai"),
    ("vi", "Vietnamese"),
    ("id", "Indonesian"),
    ("tr", "Turkish"),
    ("nl", "Dutch"),
    ("pl", "Polish"),
    ("uk", "Ukrainian"),
    ("fa", "Persian"),
    ("he", "Hebrew"),
];

pub fn output_language_name(value: &str) -> &str {
    if value == "same" {
        return "Same as spoken";
    }
    OUTPUT_LANGUAGES
        .iter()
        .find(|(code, name)| code.eq_ignore_ascii_case(value) || name.eq_ignore_ascii_case(value))
        .map(|(_, name)| *name)
        .unwrap_or(value)
}

pub fn supports_output_language(settings: &crate::Settings, value: &str) -> bool {
    if value == "same" || settings.cleanup_source.is_some() {
        return true;
    }
    if cleanup_translation_support(&settings.filter_model) == Some(TranslationSupport::Unsupported)
    {
        return false; // Transcript normalization only, including for English targets.
    }
    if settings.filter_model == "llama3.2-3b-instruct" {
        return ["en", "de", "fr", "it", "pt", "hi", "es", "th"]
            .iter()
            .any(|code| {
                output_language_name(code).eq_ignore_ascii_case(output_language_name(value))
            });
    }
    true // Qwen and custom models: further language coverage is model-dependent.
}
const CLEANUP_FILENAME: &str = "s1-mini-q4_k_m.gguf";
const EXTRA_SPEECH_MODELS: &[(&str, &str, &str, &str, u64, &str)] = &[
    (
        "qwen3-asr-0.6b",
        "Qwen3 ASR · 0.6B",
        "handy-computer/Qwen3-ASR-0.6B-gguf",
        "Qwen3-ASR-0.6B-Q8_0.gguf",
        850,
        "Hindi + English · 30 languages · accurate offline ASR · Q8_0",
    ),
    (
        "nemotron-3.5-asr-streaming-0.6b",
        "Nemotron 3.5 ASR Streaming · 0.6B",
        "handy-computer/nemotron-3.5-asr-streaming-0.6b-gguf",
        "nemotron-3.5-asr-streaming-0.6b-Q8_0.gguf",
        751,
        "Hindi + English · 32 locales · cache-aware streaming · Q8_0",
    ),
];

struct CleanupModel {
    id: &'static str,
    name: &'static str,
    repository: &'static str,
    filename: &'static str,
    size_mb: u64,
    description: &'static str,
    translation_support: TranslationSupport,
}

const CLEANUP_MODELS: &[CleanupModel] = &[
    CleanupModel {
        translation_support: TranslationSupport::Supported,
        id: "qwen3.5:0.8b-mlx",
        name: "Qwen 3.5 0.8B · MLX",
        repository: "mlx-community/Qwen3.5-0.8B-4bit",
        filename: "qwen3.5-0.8b-mlx",
        size_mb: 653,
        description: "Apple Silicon · 4-bit MLX · lightweight multilingual cleanup",
    },
    CleanupModel {
        translation_support: TranslationSupport::Supported,
        id: "qwen3.5:0.8b",
        name: "Qwen 3.5 0.8B · GGUF",
        repository: "unsloth/Qwen3.5-0.8B-GGUF",
        filename: "Qwen3.5-0.8B-Q4_K_M.gguf",
        size_mb: 533,
        description: "Lightweight multilingual cleanup · non-thinking · Q4_K_M",
    },
    CleanupModel {
        translation_support: TranslationSupport::Unsupported,
        id: DEFAULT_FILTER_MODEL,
        name: "S1-mini by Superwhisper",
        repository: "superwhisper/s1-mini-GGUF",
        filename: CLEANUP_FILENAME,
        size_mb: 484,
        description: "Recommended · specialized English transcript cleanup · Q4_K_M",
    },
    CleanupModel {
        translation_support: TranslationSupport::Supported,
        id: "qwen3-4b-instruct-2507",
        name: "Qwen 3 4B Instruct 2507",
        repository: "unsloth/Qwen3-4B-Instruct-2507-GGUF",
        filename: "Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        size_mb: 2497,
        description: "Hindi + English cleanup candidate · non-thinking · Q4_K_M",
    },
    CleanupModel {
        translation_support: TranslationSupport::Supported,
        id: "qwen2.5-1.5b-instruct",
        name: "Qwen 2.5 1.5B Instruct",
        repository: "Qwen/Qwen2.5-1.5B-Instruct-GGUF",
        filename: "qwen2.5-1.5b-instruct-q4_k_m.gguf",
        size_mb: 1117,
        description: "Lightweight multilingual instruction model · prompt-based cleanup · Q4_K_M",
    },
    CleanupModel {
        translation_support: TranslationSupport::Supported,
        id: "qwen2.5-3b-instruct",
        name: "Qwen 2.5 3B Instruct",
        repository: "Qwen/Qwen2.5-3B-Instruct-GGUF",
        filename: "qwen2.5-3b-instruct-q4_k_m.gguf",
        size_mb: 2105,
        description: "Larger multilingual instruction model · prompt-based cleanup · Q4_K_M",
    },
    CleanupModel {
        translation_support: TranslationSupport::Supported,
        id: "llama3.2-3b-instruct",
        name: "Llama 3.2 3B Instruct",
        repository: "bartowski/Llama-3.2-3B-Instruct-GGUF",
        filename: "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
        size_mb: 2019,
        description: "Meta multilingual instruction model · prompt-based cleanup · Q4_K_M",
    },
];

pub fn canonical_cleanup_id(id: &str) -> &str {
    if is_s1_model(id) {
        DEFAULT_FILTER_MODEL
    } else {
        id
    }
}

fn cleanup_model(id: &str) -> Result<&'static CleanupModel> {
    CLEANUP_MODELS
        .iter()
        .find(|model| model.id == canonical_cleanup_id(id))
        .with_context(|| {
            format!("Unsupported cleanup model '{id}'. Select a model from the cleanup catalog.")
        })
}

pub fn is_s1_model(id: &str) -> bool {
    id.eq_ignore_ascii_case(DEFAULT_FILTER_MODEL) || id == "s1-mini" || id == "s1-mini:latest"
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub size_mb: u64,
    pub description: String,
    pub installed: bool,
    /// Cleanup translation class; absent for speech or unclassified custom models.
    #[serde(default)]
    pub translation_support: Option<TranslationSupport>,
}

pub fn catalog(paths: &Paths) -> Vec<Model> {
    let mut models = vec![Model {
        id: "parakeet-unified-en-0.6b".into(),
        name: "Parakeet Unified EN · 0.6B".into(),
        kind: "speech".into(),
        translation_support: None,
        size_mb: 731,
        description: "Default · English · Q8_0 · fast local transcription".into(),
        installed: paths
            .models_dir()
            .join("parakeet-unified-en-0.6b-Q8_0.gguf")
            .exists(),
    }];
    models.extend(
        EXTRA_SPEECH_MODELS
            .iter()
            .map(|(id, name, _, filename, size, description)| Model {
                id: (*id).into(),
                name: (*name).into(),
                kind: "speech".into(),
                translation_support: None,
                size_mb: *size,
                description: (*description).into(),
                installed: paths.models_dir().join(filename).exists(),
            }),
    );
    for (id, name, size, desc) in [
        (
            "tiny",
            "Whisper Tiny",
            75,
            "Fastest · multilingual · lightest memory use",
        ),
        (
            "base",
            "Whisper Base",
            142,
            "Lightweight · multilingual · lower accuracy than larger models",
        ),
        (
            "small",
            "Whisper Small",
            466,
            "More accurate · multilingual",
        ),
        (
            "small-q8_0",
            "Whisper Small · Q8_0",
            264,
            "Multilingual · ~260 MB quantized Small · lower memory",
        ),
        (
            "large-v3",
            "Whisper Large v3",
            3095,
            "Full multilingual model · Hindi + English candidate · highest memory use",
        ),
        (
            "large-v3-turbo",
            "Whisper Turbo",
            1620,
            "High accuracy · multilingual · needs more memory",
        ),
    ] {
        models.push(Model {
            id: format!("whisper-{id}"),
            name: name.into(),
            kind: "speech".into(),
            translation_support: None,
            size_mb: size,
            description: desc.into(),
            installed: paths.models_dir().join(format!("ggml-{id}.bin")).exists(),
        });
    }
    models.extend(CLEANUP_MODELS.iter().map(|model| Model {
        id: model.id.into(),
        name: model.name.into(),
        kind: "filter".into(),
        translation_support: Some(model.translation_support),
        size_mb: model.size_mb,
        description: model.description.into(),
        installed: if model.id.ends_with("-mlx") {
            paths
                .models_dir()
                .join(model.filename)
                .join(".ready")
                .exists()
        } else {
            paths.models_dir().join(model.filename).exists()
        },
    }));
    if let Ok(settings) = read_settings(paths) {
        if settings.cleanup_source.is_some() {
            let location = cleanup_location(paths, &settings).ok();
            models.push(Model {
                id: "custom".into(),
                name: "Custom cleanup model".into(),
                kind: "filter".into(),
                translation_support: None,
                size_mb: 0,
                description: format!(
                    "{} · {}",
                    if settings.uses_mlx() { "MLX" } else { "GGUF" },
                    settings.cleanup_source.as_deref().unwrap_or_default()
                ),
                installed: location.is_some_and(|path| {
                    if settings.uses_mlx() {
                        path.join("config.json").exists()
                    } else {
                        path.is_file()
                    }
                }),
            });
        }
    }
    models
}

pub fn speech_path(paths: &Paths, id: &str) -> Result<std::path::PathBuf> {
    if !catalog(paths)
        .iter()
        .any(|m| m.id == id && m.kind == "speech")
    {
        bail!("Unknown speech model: {id}");
    }
    if id == "parakeet-unified-en-0.6b" {
        return Ok(paths
            .models_dir()
            .join("parakeet-unified-en-0.6b-Q8_0.gguf"));
    }
    if let Some((_, _, _, filename, _, _)) = EXTRA_SPEECH_MODELS.iter().find(|model| model.0 == id)
    {
        return Ok(paths.models_dir().join(filename));
    }
    Ok(paths
        .models_dir()
        .join(format!("ggml-{}.bin", id.trim_start_matches("whisper-"))))
}

pub fn cleanup_path(paths: &Paths, id: &str) -> Result<std::path::PathBuf> {
    if id == "custom" {
        let settings = read_settings(paths)?;
        if settings.cleanup_source.is_none() {
            bail!("Custom cleanup_source is missing");
        }
        return cleanup_location(paths, &settings);
    }
    Ok(paths.models_dir().join(cleanup_model(id)?.filename))
}

fn read_settings(paths: &Paths) -> Result<crate::Settings> {
    if paths.settings_path().exists() {
        Ok(serde_json::from_slice(&std::fs::read(
            paths.settings_path(),
        )?)?)
    } else {
        Ok(crate::Settings::default())
    }
}

pub fn cleanup_location(paths: &Paths, settings: &crate::Settings) -> Result<std::path::PathBuf> {
    if let Some(source) = &settings.cleanup_source {
        let path = std::path::PathBuf::from(source);
        if path.is_absolute() {
            return Ok(path);
        }
        let hash = source.bytes().fold(14695981039346656037u64, |hash, byte| {
            (hash ^ byte as u64).wrapping_mul(1099511628211)
        });
        return Ok(paths.models_dir().join(if settings.uses_mlx() {
            format!("custom-{hash:x}-mlx")
        } else {
            format!("custom-{hash:x}.gguf")
        }));
    }
    cleanup_path(paths, &settings.filter_model)
}

pub async fn list(paths: &Paths) -> Result<Vec<Model>> {
    Ok(catalog(paths))
}

/// Cached weight files, including a partial transfer, without touching anything else.
pub fn is_installed(paths: &Paths, id: &str) -> bool {
    let id = canonical_cleanup_id(id);
    catalog(paths)
        .iter()
        .any(|model| model.id == id && model.installed)
}

fn model_file(paths: &Paths, id: &str) -> Result<std::path::PathBuf> {
    if catalog(paths)
        .iter()
        .any(|model| model.id == id && model.kind == "speech")
    {
        speech_path(paths, id)
    } else {
        cleanup_path(paths, id)
    }
}

/// Features that require this catalog model's cache to remain available.
pub fn cache_users(settings: &crate::Settings, id: &str) -> Vec<&'static str> {
    let id = canonical_cleanup_id(id);
    let mut users = Vec::new();
    if id == settings.speech_model {
        users.push("speech_model");
    }
    if settings.filter_enabled
        && settings.cleanup_source.is_none()
        && id == canonical_cleanup_id(&settings.filter_model)
    {
        users.push("filter_model");
    }
    if settings.daily_structure_enabled
        && id == canonical_cleanup_id(&settings.daily_structure_model)
    {
        users.push("daily_structure_model");
    }
    users
}

/// Remove a cached model so a later selection can download it again.
pub fn delete(paths: &Paths, id: &str) -> Result<()> {
    let id = canonical_cleanup_id(id);
    if id == "custom" {
        bail!("Custom model files are managed at their source; select another model to stop using them");
    }
    if !catalog(paths).iter().any(|model| model.id == id) {
        bail!("Unknown catalog model: {id}");
    }
    let file = model_file(paths, id)?;
    let part = file.with_extension("part");
    for path in [file, part] {
        if path.exists() {
            if path.is_dir() {
                std::fs::remove_dir_all(path)?;
            } else {
                std::fs::remove_file(path)?;
            }
        }
    }
    Ok(())
}

/// Drop an interrupted transfer so the next download starts from a clean file.
pub fn remove_partial(paths: &Paths, id: &str) -> Result<()> {
    let file = model_file(paths, canonical_cleanup_id(id))?;
    let part = file.with_extension("part");
    if part.exists() {
        std::fs::remove_file(part)?;
    }
    Ok(())
}

pub async fn download(paths: &Paths, id: &str, events: &broadcast::Sender<Event>) -> Result<()> {
    let id = canonical_cleanup_id(id);
    let model = catalog(paths)
        .into_iter()
        .find(|m| m.id == id)
        .context("Unknown catalog model")?;
    let dest = if model.kind == "speech" {
        speech_path(paths, id)?
    } else {
        cleanup_path(paths, id)?
    };
    let settings = read_settings(paths)?;
    let mlx = id.ends_with("-mlx") || id == "custom" && settings.uses_mlx();
    if mlx {
        if dest.join(".ready").exists()
            || dest.join("config.json").exists()
                && settings
                    .cleanup_source
                    .as_ref()
                    .is_some_and(|s| std::path::Path::new(s).is_absolute())
        {
            return Ok(());
        }
        let repository = if id == "custom" {
            settings
                .cleanup_source
                .clone()
                .context("Custom model source missing")?
        } else {
            cleanup_model(id)?.repository.into()
        };
        let _ = events.send(Event::Download {
            id: id.into(),
            completed: 0,
            total: model.size_mb * 1_000_000,
        });
        let paths = paths.clone();
        let destination = dest.clone();
        tokio::task::spawn_blocking(move || {
            crate::mlx::download(&paths, &settings, &repository, &destination)
        })
        .await??;
        let _ = events.send(Event::Download {
            id: id.into(),
            completed: model.size_mb * 1_000_000,
            total: model.size_mb * 1_000_000,
        });
        let _ = events.send(Event::ModelsChanged);
        return Ok(());
    }
    if dest.exists() {
        return Ok(());
    }

    let part = dest.with_extension("part");
    let result: Result<()> = async {
        let source = match id {
            "custom" => {
                let source = settings.cleanup_source.clone().context("Custom model source missing")?;
                if !source.starts_with("https://") { bail!("Custom GGUF file not found: {source}"); }
                source
            }
            id if EXTRA_SPEECH_MODELS.iter().any(|model| model.0 == id) => {
                let (_, _, repository, filename, _, _) = EXTRA_SPEECH_MODELS.iter().find(|model| model.0 == id).unwrap();
                format!("https://huggingface.co/{repository}/resolve/main/{filename}")
            }
            "parakeet-unified-en-0.6b" => {
                format!("https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/main/{}", dest.file_name().unwrap().to_string_lossy())
            }
            id if id.starts_with("whisper-") => {
                format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}", dest.file_name().unwrap().to_string_lossy())
            }
            _ => {
                let model = cleanup_model(id)?;
                format!("https://huggingface.co/{}/resolve/main/{}", model.repository, model.filename)
            },
        };
        let response = reqwest::Client::builder()
            // Multi-gigabyte multilingual models can take more than ten minutes
            // on a normal connection. Bound stalls separately from total time.
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(120))
            .timeout(std::time::Duration::from_secs(3600))
            .build()?
            .get(source)
            .send()
            .await?
            .error_for_status()?;
        let expected_bytes = response.content_length();
        let total = expected_bytes.unwrap_or(model.size_mb * 1_000_000);
        let mut stream = response.bytes_stream();
        let mut file = tokio::fs::File::create(&part).await?;
        let mut done = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            file.write_all(&chunk).await?;
            done += chunk.len() as u64;
            let _ = events.send(Event::Download { id: id.into(), completed: done, total });
        }
        file.flush().await?;
        if done == 0 || expected_bytes.is_some_and(|expected| done != expected) {
            bail!("Incomplete model download: received {done} of {total} bytes");
        }
        drop(file);
        tokio::fs::rename(&part, &dest).await?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(part).await;
    }
    result?;
    let _ = events.send(Event::ModelsChanged);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translation_classes_and_warnings_match_cleanup_models_only() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::in_directory(directory.path());
        for model in catalog(&paths) {
            if model.kind == "speech" {
                assert_eq!(model.translation_support, None);
                continue;
            }
            let unsupported = is_s1_model(&model.id);
            assert_eq!(
                model.translation_support,
                Some(if unsupported {
                    TranslationSupport::Unsupported
                } else {
                    TranslationSupport::Supported
                })
            );
            let settings = crate::Settings {
                filter_model: model.id,
                ..Default::default()
            };
            assert_eq!(translation_warning(&settings).is_some(), unsupported);
        }
        let custom = crate::Settings {
            cleanup_source: Some("/custom/model.gguf".into()),
            ..Default::default()
        };
        assert!(translation_warning(&custom).is_none());
    }

    #[test]
    fn delete_removes_only_the_requested_model() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::in_directory(directory.path());
        paths.ensure_dirs().unwrap();
        assert!(!is_installed(&paths, "whisper-tiny"));
        assert!(delete(&paths, "whisper-tiny").is_err() || !is_installed(&paths, "whisper-tiny"));
        let tiny = paths.models_dir().join("ggml-tiny.bin");
        std::fs::write(&tiny, b"cached").unwrap();
        std::fs::write(paths.models_dir().join(CLEANUP_FILENAME), b"other").unwrap();
        assert!(is_installed(&paths, "whisper-tiny"));
        assert!(delete(&paths, "whisper-tiny").is_ok());
        assert!(!tiny.exists());
        assert!(
            is_installed(&paths, DEFAULT_FILTER_MODEL),
            "other models stay cached"
        );
        assert!(delete(&paths, "not-a-model").is_err());
        remove_partial(&paths, "whisper-tiny").unwrap();
    }

    #[test]
    fn cleanup_catalog_paths_are_distinct_and_aliases_keep_existing_cache() {
        let paths = Paths::in_directory("unused-model-catalog-test");
        let filters: Vec<_> = catalog(&paths)
            .into_iter()
            .filter(|m| m.kind == "filter")
            .collect();
        assert_eq!(filters.len(), 7);
        let files: std::collections::HashSet<_> = filters
            .iter()
            .map(|m| cleanup_path(&paths, &m.id).unwrap())
            .collect();
        assert_eq!(files.len(), filters.len());
        assert_eq!(
            cleanup_path(&paths, "s1-mini").unwrap(),
            cleanup_path(&paths, DEFAULT_FILTER_MODEL).unwrap()
        );
        assert!(cleanup_path(&paths, "../../outside.gguf").is_err());
        assert!(cleanup_path(&paths, "whisper-base").is_err());
    }
}

#[cfg(test)]
mod cache_usage_tests {
    use super::*;

    #[test]
    fn disabled_features_do_not_protect_their_cache() {
        let mut settings = crate::Settings::default();
        settings.filter_model = "qwen3.5:0.8b".into();
        settings.daily_structure_model = "qwen2.5-1.5b-instruct".into();
        settings.filter_enabled = true;
        settings.daily_structure_enabled = true;
        assert_eq!(
            cache_users(&settings, "qwen2.5-1.5b-instruct"),
            vec!["daily_structure_model"]
        );
        assert_eq!(
            cache_users(&settings, canonical_cleanup_id(&settings.filter_model)),
            vec!["filter_model"]
        );
        settings.daily_structure_enabled = false;
        settings.filter_enabled = false;
        assert!(cache_users(&settings, "qwen2.5-1.5b-instruct").is_empty());
        assert!(cache_users(&settings, &settings.filter_model).is_empty());
        assert_eq!(
            cache_users(&settings, &settings.speech_model),
            vec!["speech_model"]
        );
    }
}
