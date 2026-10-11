//! Exercise the same speech gate used by capture without loading an ASR model.
use anyhow::{bail, Result};
use contextd_core::{audio, Settings};
use serde::Serialize;

#[derive(Default, Serialize)]
struct Stats {
    source: String,
    seconds: f64,
    peak_rms: f32,
    peak_voice_probability: f32,
    speech_starts: usize,
    segments: usize,
    captured_seconds: f64,
}
impl Stats {
    fn feed(&mut self, gate: &mut audio::Segmenter, samples: &[f32]) -> Result<()> {
        self.seconds += samples.len() as f64 / 16000.0;
        for frame in samples.chunks(256) {
            self.peak_rms = self.peak_rms.max(audio::rms(frame));
            let active = gate.is_active();
            for segment in gate.push(frame)? {
                self.segments += 1;
                self.captured_seconds += segment.samples.len() as f64 / 16000.0;
            }
            if !active && gate.is_active() {
                self.speech_starts += 1;
            }
            self.peak_voice_probability = self
                .peak_voice_probability
                .max(gate.last_speech_probability());
        }
        Ok(())
    }
    fn finish(&mut self, gate: &mut audio::Segmenter) -> Result<()> {
        if let Some(segment) = gate.flush()? {
            self.segments += 1;
            self.captured_seconds += segment.samples.len() as f64 / 16000.0;
        }
        println!("{}", serde_json::to_string(self)?);
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.is_empty() {
        bail!("Usage: check_vad FILE.wav [...] | --microphone [SECONDS] [DEVICE_ID]");
    }
    let settings = Settings::default();
    if args[0] == "--microphone" {
        let seconds = args
            .get(1)
            .map(|s| s.parse::<u64>())
            .transpose()?
            .unwrap_or(10);
        let (sender, mut receiver) = tokio::sync::mpsc::channel(256);
        let (errors, mut error_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut capture = audio::Capture::start(args.get(2).cloned(), sender, errors)?;
        let mut gate = audio::Segmenter::new(settings)?;
        let mut stats = Stats {
            source: capture.description.clone(),
            ..Stats::default()
        };
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(seconds));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                Some(error) = error_rx.recv() => bail!("{error}"),
                Some(samples) = receiver.recv() => stats.feed(&mut gate, &samples)?,
            }
        }
        capture.stop();
        while let Ok(samples) = receiver.try_recv() {
            stats.feed(&mut gate, &samples)?;
        }
        stats.finish(&mut gate)?;
    } else {
        for file in args {
            let samples = audio::read_wav(std::path::Path::new(&file))?;
            let mut gate = audio::Segmenter::new(settings.clone())?;
            let mut stats = Stats {
                source: file,
                ..Stats::default()
            };
            stats.feed(&mut gate, &samples)?;
            stats.finish(&mut gate)?;
        }
    }
    Ok(())
}
