use crate::{Event, Paths};
use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::{io::AsyncWriteExt, sync::broadcast};

pub const DEFAULT_FILTER_MODEL: &str = "hf.co/superwhisper/s1-mini-GGUF:Q4_K_M";
const CLEANUP_FILENAME: &str = "s1-mini-q4_k_m.gguf";

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
        id: DEFAULT_FILTER_MODEL,
        name: "S1-mini by Superwhisper",
        repository: "superwhisper/s1-mini-GGUF",
        filename: CLEANUP_FILENAME,
        size_mb: 484,
        description: "Recommended · specialized English transcript cleanup · Q4_K_M",
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
            "Balanced · multilingual · recommended starting point",
        ),
        (
            "small",
            "Whisper Small",
            466,
            "More accurate · multilingual",
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
        installed: paths.models_dir().join(model.filename).exists(),
    }));
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
    Ok(paths
        .models_dir()
        .join(format!("ggml-{}.bin", id.trim_start_matches("whisper-"))))
}

pub fn cleanup_path(paths: &Paths, id: &str) -> Result<std::path::PathBuf> {
    Ok(paths.models_dir().join(cleanup_model(id)?.filename))
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
    if !catalog(paths).iter().any(|model| model.id == id) {
        bail!("Unknown catalog model: {id}");
    }
    let file = model_file(paths, id)?;
    let part = file.with_extension("part");
    for path in [file, part] {
        if path.exists() {
            std::fs::remove_file(path)?;
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
    if dest.exists() {
        return Ok(());
    }

    let part = dest.with_extension("part");
    let result: Result<()> = async {
        let source = match id {
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
            .timeout(std::time::Duration::from_secs(600))
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
        assert_eq!(filters.len(), 4);
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
