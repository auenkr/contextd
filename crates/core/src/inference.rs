use crate::{models, Paths, Settings};
use anyhow::{bail, Context, Result};
use llama_cpp_2::{
    context::params::LlamaContextParams,
    llama_backend::LlamaBackend,
    llama_batch::LlamaBatch,
    model::{params::LlamaModelParams, LlamaModel},
};
use std::time::Instant;
use transcribe_cpp::{Model, RunOptions, StreamOptions};

fn configure_native_logging() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        // Install before either runtime loads a model or initializes Metal.
        transcribe_cpp::disable_logging();
        llama_cpp_2::send_logs_to_tracing(
            llama_cpp_2::LogOptions::default().with_logs_enabled(false),
        );
    });
}

// Verbatim training protocol: https://huggingface.co/superwhisper/s1-mini-GGUF
// S1-mini is not an instruction-following chat model. Do not add generic
// instructions or few-shot turns; its non-thinking assistant prefix is required.
pub const S1_SYSTEM_PROMPT: &str = "You are a text normalizer for speech-to-text transcripts. The input begins with a control line specifying the styling, structure, and context settings; clean the transcript to match those settings and output only the cleaned text.";

fn s1_prompt(text: &str) -> String {
    format!("<|im_start|>system\n{S1_SYSTEM_PROMPT}<|im_end|>\n\
        <|im_start|>user\n[Styling: semi-formal] [Structure: prose] [Context: general]\n{text}<|im_end|>\n\
        <|im_start|>assistant\n<think>\n\n</think>\n\n")
}

// Prefer a sentence/paragraph boundary near the middle. Fall back to whitespace
// and then a UTF-8 boundary so even one enormous word can be split safely.
fn cleanup_split(text: &str) -> Option<(&str, &str)> {
    let middle = text.len() / 2;
    let candidates = |sentences_only: bool| {
        text.char_indices()
            .filter_map(move |(i, c)| {
                (i > 0
                    && c.is_whitespace()
                    && (!sentences_only || c == '\n' || text[..i].ends_with(['.', '!', '?'])))
                .then_some(i)
            })
            .min_by_key(|i| i.abs_diff(middle))
    };
    let index = candidates(true).or_else(|| candidates(false)).or_else(|| {
        text.char_indices()
            .map(|(i, _)| i)
            .filter(|i| *i > 0)
            .min_by_key(|i| i.abs_diff(middle))
    })?;
    Some(text.split_at(index))
}

pub trait SpeechToText: Send {
    fn transcribe(&mut self, samples: &[f32], settings: &Settings) -> Result<String>;
    fn unload(&mut self);
}

/// transcribe.cpp dispatches to Parakeet or Whisper based on the model file.
pub struct LocalSpeechEngine {
    paths: Paths,
    loaded: Option<(String, Model)>,
    pub last_used: Instant,
}
impl LocalSpeechEngine {
    pub fn new(paths: Paths) -> Self {
        configure_native_logging();
        Self {
            paths,
            loaded: None,
            last_used: Instant::now(),
        }
    }
    pub fn unload_if_idle(&mut self, seconds: u64) -> bool {
        if seconds > 0 && self.loaded.is_some() && self.last_used.elapsed().as_secs() >= seconds {
            self.unload();
            true
        } else {
            false
        }
    }
}
impl SpeechToText for LocalSpeechEngine {
    fn transcribe(&mut self, samples: &[f32], settings: &Settings) -> Result<String> {
        if self.loaded.as_ref().map(|(id, _)| id) != Some(&settings.speech_model) {
            self.unload();
            let path = models::speech_path(&self.paths, &settings.speech_model)?;
            if !path.exists() {
                bail!("Download {} before recording", settings.speech_model);
            }
            self.loaded = Some((
                settings.speech_model.clone(),
                Model::load(path.to_str().context("Invalid model path")?)?,
            ));
        }
        let mut session = self.loaded.as_ref().unwrap().1.session()?;
        let options = RunOptions {
            language: if settings.speech_model.starts_with("parakeet-") {
                Some("en".into())
            } else if settings.language == "auto" {
                None
            } else {
                Some(settings.language.clone())
            },
            ..Default::default()
        };
        // Keep one recognition session across long utterances. Streaming models
        // bound their internal audio windows without creating note boundaries.
        // Short utterances retain the existing whole-buffer recognition path.
        let text = if samples.len() > 30 * 16000
            && self
                .loaded
                .as_ref()
                .unwrap()
                .1
                .capabilities()
                .supports_streaming
        {
            let mut stream = session.stream(&options, &StreamOptions::default())?;
            for chunk in samples.chunks(16000) {
                stream.feed(chunk)?;
            }
            stream.finalize()?;
            stream.snapshot().text
        } else {
            session.run(samples, &options)?.text
        };
        self.last_used = Instant::now();
        Ok(text.trim().to_owned())
    }
    fn unload(&mut self) {
        self.loaded = None;
    }
}

pub struct LocalCleanupEngine {
    backend: Option<LlamaBackend>,
    loaded: Option<(std::path::PathBuf, LlamaModel)>,
    last_used: Instant,
}

impl LocalCleanupEngine {
    pub fn new() -> Self {
        configure_native_logging();
        Self {
            backend: None,
            loaded: None,
            last_used: Instant::now(),
        }
    }

    pub fn refine(&mut self, paths: &Paths, settings: &Settings, text: &str) -> Result<String> {
        if !settings.filter_enabled || text.trim().is_empty() {
            return Ok(text.into());
        }
        let path = models::cleanup_path(paths, &settings.filter_model)?;
        if !path.exists() {
            bail!("Download S1-mini before enabling speech cleanup");
        }
        if self.loaded.as_ref().map(|(loaded, _)| loaded) != Some(&path) {
            self.unload();
            let backend =
                LlamaBackend::init().context("Could not initialize local GGUF runtime")?;
            let params = LlamaModelParams::default().with_n_gpu_layers(gpu_layers());
            let model = LlamaModel::load_from_file(&backend, &path, &params)
                .context("Could not load the S1-mini cleanup model")?;
            self.backend = Some(backend);
            self.loaded = Some((path, model));
        }
        let backend = self
            .backend
            .as_ref()
            .context("Local inference runtime is not initialized")?;
        let model = &self.loaded.as_ref().unwrap().1;
        let mut context = model.new_context(
            backend,
            LlamaContextParams::default().with_n_ctx(std::num::NonZeroU32::new(4096)),
        )?;
        let vocab = model.vocab();
        let input = vocab.tokenize(s1_prompt(text).as_bytes(), true, true);
        // Input and output share the context. Reserve enough room for a cleaned
        // transcript of similar length, including the fixed prompt overhead.
        let prompt_tokens = vocab.tokenize(s1_prompt("").as_bytes(), true, true).len();
        let transcript_tokens = input.len().saturating_sub(prompt_tokens);
        if input.len() + transcript_tokens + 128 >= context.n_ctx() as usize {
            drop(context);
            return self.refine_chunks(paths, settings, text);
        }
        let mut batch = LlamaBatch::new(input.len().max(1), 1);
        for (position, token) in input.iter().enumerate() {
            batch.add(*token, position as i32, &[0], position + 1 == input.len())?;
        }
        context.decode(&mut batch)?;

        let mut output = Vec::new();
        let mut position = input.len();
        while position < context.n_ctx() as usize {
            let token = context.token_data_array().sample_token_greedy();
            if vocab.is_eog(token) {
                break;
            }
            output.extend(vocab.token_to_piece(token, false, None));
            batch.clear();
            batch.add(token, position as i32, &[0], true)?;
            context.decode(&mut batch)?;
            position += 1;
        }
        if position >= context.n_ctx() as usize {
            drop(context);
            return self.refine_chunks(paths, settings, text);
        }
        let cleaned = String::from_utf8_lossy(&output).trim().to_owned();
        self.last_used = Instant::now();
        // S1-mini intentionally returns empty text for filler-only input.
        Ok(cleaned)
    }

    fn refine_chunks(&mut self, paths: &Paths, settings: &Settings, text: &str) -> Result<String> {
        let (left, right) = cleanup_split(text)
            .context("Cleanup cannot fit even the smallest input; original preserved")?;
        let left = self.refine(paths, settings, left.trim())?;
        let right = self.refine(paths, settings, right.trim())?;
        Ok([left, right]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"))
    }

    pub fn unload_if_idle(&mut self, seconds: u64) -> bool {
        if seconds > 0 && self.loaded.is_some() && self.last_used.elapsed().as_secs() >= seconds {
            self.unload();
            true
        } else {
            false
        }
    }

    pub fn unload(&mut self) {
        self.loaded = None;
        self.backend = None;
    }
}

impl Default for LocalCleanupEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "macos")]
fn gpu_layers() -> u32 {
    99
}
#[cfg(not(target_os = "macos"))]
fn gpu_layers() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_split_preserves_text_and_prefers_sentence_boundaries() {
        let text = "First thought. The next thought continues with more detail.";
        let (left, right) = cleanup_split(text).unwrap();
        assert_eq!(left, "First thought.");
        assert_eq!(format!("{left}{right}"), text);
        for text in ["你好世界", "averylongword", "one two three four"] {
            let (left, right) = cleanup_split(text).unwrap();
            assert!(!left.is_empty());
            assert!(!right.is_empty());
            assert_eq!(format!("{left}{right}"), text);
        }
        assert!(cleanup_split("a").is_none());
    }
    #[test]
    fn s1_prompt_keeps_the_model_training_format() {
        let prompt = s1_prompt("Ship Tuesday, no, Wednesday");
        assert!(prompt.starts_with(&format!(
            "<|im_start|>system\n{S1_SYSTEM_PROMPT}<|im_end|>\n"
        )));
        assert!(prompt.contains("[Styling: semi-formal] [Structure: prose] [Context: general]\n"));
        assert!(prompt.ends_with("<|im_start|>assistant\n<think>\n\n</think>\n\n"));
    }

    #[test]
    fn cleanup_disabled_returns_input_without_loading_model() {
        let paths = Paths::in_directory(std::path::PathBuf::from("unused"));
        let mut engine = LocalCleanupEngine::new();
        assert_eq!(
            engine
                .refine(&paths, &Settings::default(), "Um keep this.")
                .unwrap(),
            "Um keep this."
        );
    }
}
