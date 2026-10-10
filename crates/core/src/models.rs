use crate::{Event, Paths};
use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::{io::AsyncWriteExt, sync::broadcast};

pub const DEFAULT_FILTER_MODEL: &str = "hf.co/superwhisper/s1-mini-GGUF:Q4_K_M";
const CLEANUP_FILENAME: &str = "s1-mini-q4_k_m.gguf";

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
    models.push(Model {
        id: DEFAULT_FILTER_MODEL.into(),
        name: "S1-mini by Superwhisper".into(),
        kind: "filter".into(),
        size_mb: 484,
        description: format!(
            "Transcript cleanup · English · Q4_K_M · runs inside {}",
            crate::config::PRODUCT_NAME
        ),
        installed: paths.models_dir().join(CLEANUP_FILENAME).exists(),
    });
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
    if !is_s1_model(id) {
        bail!("Unsupported cleanup model '{id}'. Select S1-mini from the model catalog.");
    }
    Ok(paths.models_dir().join(CLEANUP_FILENAME))
}

pub async fn list(paths: &Paths) -> Result<Vec<Model>> {
    Ok(catalog(paths))
}

pub async fn download(paths: &Paths, id: &str, events: &broadcast::Sender<Event>) -> Result<()> {
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
            _ => format!("https://huggingface.co/superwhisper/s1-mini-GGUF/resolve/main/{CLEANUP_FILENAME}"),
        };
        let response = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()?
            .get(source)
            .send()
            .await?
            .error_for_status()?;
        let total = response.content_length().unwrap_or(model.size_mb * 1_000_000);
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
        if done == 0 || (total > 0 && done != total) {
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
