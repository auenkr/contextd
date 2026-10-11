use crate::{Event, Paths};
use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::{io::AsyncWriteExt, sync::broadcast};

pub const DEFAULT_FILTER_MODEL: &str = "hf.co/superwhisper/s1-mini-GGUF:Q4_K_M";
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
}

const CLEANUP_MODELS: &[CleanupModel] = &[
    CleanupModel {
        id: "qwen3.5:0.8b-mlx",
        name: "Qwen 3.5 0.8B · MLX",
        repository: "mlx-community/Qwen3.5-0.8B-4bit",
        filename: "qwen3.5-0.8b-mlx",
        size_mb: 653,
        description: "Apple Silicon · 4-bit MLX · lightweight multilingual cleanup",
    },
    CleanupModel {
        id: "qwen3.5:0.8b",
        name: "Qwen 3.5 0.8B · GGUF",
        repository: "unsloth/Qwen3.5-0.8B-GGUF",
        filename: "Qwen3.5-0.8B-Q4_K_M.gguf",
        size_mb: 533,
        description: "Lightweight multilingual cleanup · non-thinking · Q4_K_M",
    },
    CleanupModel {
        id: DEFAULT_FILTER_MODEL,
        name: "S1-mini by Superwhisper",
        repository: "superwhisper/s1-mini-GGUF",
        filename: CLEANUP_FILENAME,
        size_mb: 484,
        description: "Recommended · specialized English transcript cleanup · Q4_K_M",
    },
    CleanupModel {
        id: "qwen3-4b-instruct-2507",
        name: "Qwen 3 4B Instruct 2507",
        repository: "unsloth/Qwen3-4B-Instruct-2507-GGUF",
        filename: "Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        size_mb: 2497,
        description: "Hindi + English cleanup candidate · non-thinking · Q4_K_M",
    },
    CleanupModel {
        id: "qwen2.5-1.5b-instruct",
        name: "Qwen 2.5 1.5B Instruct",
        repository: "Qwen/Qwen2.5-1.5B-Instruct-GGUF",
        filename: "qwen2.5-1.5b-instruct-q4_k_m.gguf",
        size_mb: 1117,
        description: "Lightweight multilingual instruction model · prompt-based cleanup · Q4_K_M",
    },
    CleanupModel {
        id: "qwen2.5-3b-instruct",
        name: "Qwen 2.5 3B Instruct",
        repository: "Qwen/Qwen2.5-3B-Instruct-GGUF",
        filename: "qwen2.5-3b-instruct-q4_k_m.gguf",
        size_mb: 2105,
        description: "Larger multilingual instruction model · prompt-based cleanup · Q4_K_M",
    },
    CleanupModel {
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
}

pub fn catalog(paths: &Paths) -> Vec<Model> {
    let mut models = vec![Model {
        id: "parakeet-unified-en-0.6b".into(),
        name: "Parakeet Unified EN · 0.6B".into(),
        kind: "speech".into(),
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
            size_mb: size,
            description: desc.into(),
            installed: paths.models_dir().join(format!("ggml-{id}.bin")).exists(),
        });
    }
    models.extend(CLEANUP_MODELS.iter().map(|model| Model {
        id: model.id.into(),
        name: model.name.into(),
        kind: "filter".into(),
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
