use crate::Settings;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct DeviceProfile {
    pub platform: String,
    pub architecture: String,
    pub cpu: String,
    pub memory_gb: f64,
    pub available_memory_gb: f64,
    pub apple_silicon: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct Recommendation {
    pub speech_model: String,
    pub cleanup_model: String,
    pub reason: String,
}
impl DeviceProfile {
    pub fn detect() -> Self {
        let mut system = sysinfo::System::new();
        system.refresh_memory();
        system.refresh_cpu_all();
        Self {
            platform: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
            cpu: system
                .cpus()
                .first()
                .map(|cpu| cpu.brand().to_owned())
                .unwrap_or_default(),
            memory_gb: system.total_memory() as f64 / 1073741824.0,
            available_memory_gb: system.available_memory() as f64 / 1073741824.0,
            apple_silicon: cfg!(all(target_os = "macos", target_arch = "aarch64")),
        }
    }
    pub fn recommend(&self, settings: &Settings) -> Recommendation {
        let constrained = self.memory_gb < 8.0 || self.available_memory_gb < 1.5;
        let speech = if constrained {
            "whisper-small-q8_0"
        } else if matches!(settings.speech_language(), "hi" | "hi-en") {
            "qwen3-asr-0.6b"
        } else {
            "nemotron-3.5-asr-streaming-0.6b"
        };
        let cleanup = "qwen3.5:0.8b";
        Recommendation {
            speech_model: speech.into(),
            cleanup_model: cleanup.into(),
            reason: format!(
                "{} · {:.0} GB RAM ({:.1} GB available) · preferred language {}. {} {}",
                self.cpu,
                self.memory_gb,
                self.available_memory_gb,
                settings.speech_language(),
                if constrained {
                    "Quantized Whisper Small minimizes memory; larger ASR models are more accurate for Hindi."
                } else {
                    "Qwen3 ASR is preferred for Hindi/mixed speech in our small sample; Nemotron 3.5 is the English/streaming choice. Neither guarantees English words remain in Latin script."
                },
                if self.apple_silicon {
                    "GGUF 0.8B is the lightweight cleanup choice (9/13 English, 5/10 mixed). Apple Silicon MLX is supported but scored 8/13 and 5/10; 4B remains the accuracy-focused alternative."
                } else {
                    "GGUF 0.8B is the portable CPU cleanup option; MLX requires Apple Silicon."
                }
            ),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recommendations_respect_memory_and_mlx_platform_support() {
        let mut device = DeviceProfile {
            platform: "macos".into(),
            architecture: "aarch64".into(),
            cpu: "Apple M4".into(),
            memory_gb: 16.0,
            available_memory_gb: 8.0,
            apple_silicon: true,
        };
        assert_eq!(
            device.recommend(&Settings::default()).cleanup_model,
            "qwen3.5:0.8b"
        );
        device.apple_silicon = false;
        device.memory_gb = 4.0;
        let result = device.recommend(&Settings::default());
        assert_eq!(result.speech_model, "whisper-small-q8_0");
        assert_eq!(result.cleanup_model, "qwen3.5:0.8b");
        device.memory_gb = 16.0;
        let settings = Settings {
            preferred_language: "hi-en".into(),
            ..Settings::default()
        };
        assert_eq!(device.recommend(&settings).speech_model, "qwen3-asr-0.6b");
    }
}
