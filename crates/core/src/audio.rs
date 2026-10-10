use crate::Settings;
use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
};
use tokio::sync::mpsc;

pub struct Capture {
    pub description: String,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
#[derive(Clone, serde::Serialize)]
pub struct Microphone {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

pub fn microphones() -> Result<Vec<Microphone>> {
    let host = cpal::default_host();
    let default = host.default_input_device().and_then(|d| d.name().ok());
    Ok(host
        .input_devices()?
        .enumerate()
        .map(|(index, device)| {
            let name = device
                .name()
                .unwrap_or_else(|_| "Unknown microphone".into());
            Microphone {
                id: format!("{index}:{name}"),
                is_default: default.as_ref() == Some(&name),
                name,
            }
        })
        .collect())
}
impl Capture {
    pub fn start(
        microphone: Option<String>,
        sender: mpsc::Sender<Vec<f32>>,
        errors: mpsc::UnboundedSender<String>,
    ) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = thread::spawn(move || {
            let result = (|| -> Result<()> {
                ensure_microphone_access()?;
                let host = cpal::default_host();
                let device = if let Some(id) = microphone {
                    host.input_devices()?
                        .enumerate()
                        .find(|(index, d)| {
                            format!("{index}:{}", d.name().unwrap_or_default()) == id
                        })
                        .map(|(_, d)| d)
                        .context("Selected microphone is disconnected; select another input")?
                } else {
                    host.default_input_device().context("No microphone found")?
                };
                let config = device.default_input_config()?;
                let channels = config.channels() as usize;
                let rate = config.sample_rate().0;
                let stream_errors = errors.clone();
                let err = move |e: cpal::StreamError| {
                    let _ = stream_errors.send(e.to_string());
                };
                let stream = match config.sample_format() {
                    cpal::SampleFormat::F32 => device.build_input_stream(
                        &config.clone().into(),
                        callback::<f32>(sender, errors.clone(), channels, rate),
                        err,
                        None,
                    )?,
                    cpal::SampleFormat::I16 => device.build_input_stream(
                        &config.clone().into(),
                        callback::<i16>(sender, errors.clone(), channels, rate),
                        err,
                        None,
                    )?,
                    cpal::SampleFormat::U16 => device.build_input_stream(
                        &config.clone().into(),
                        callback::<u16>(sender, errors.clone(), channels, rate),
                        err,
                        None,
                    )?,
                    format => bail!("Unsupported microphone format: {format:?}"),
                };
                stream.play()?;
                let description = format!(
                    "{} ({rate} Hz, {channels} channels)",
                    device
                        .name()
                        .unwrap_or_else(|_| "Unknown microphone".into())
                );
                let _ = ready_tx.send(Ok(description));
                while !flag.load(Ordering::Relaxed) {
                    thread::sleep(std::time::Duration::from_millis(50));
                }
                drop(stream);
                Ok(())
            })();
            if let Err(e) = result {
                let _ = ready_tx.send(Err(e.to_string()));
            }
        });
        match ready_rx.recv()? {
            Ok(description) => Ok(Self {
                description,
                stop,
                thread: Some(thread),
            }),
            Err(e) => {
                let _ = thread.join();
                bail!("{e}")
            }
        }
    }
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(target_os = "macos")]
fn ensure_microphone_access() -> Result<()> {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

    // AVMediaTypeAudio is an immutable framework constant. Authorization APIs
    // accept it from any thread; the completion block is copied by AVFoundation.
    let media_type = unsafe { AVMediaTypeAudio }.context("macOS audio media type unavailable")?;
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) };
    if status == AVAuthorizationStatus::Authorized {
        return Ok(());
    }
    if status == AVAuthorizationStatus::NotDetermined {
        let (sender, receiver) = std::sync::mpsc::channel();
        let completion = RcBlock::new(move |granted: Bool| {
            let _ = sender.send(granted.as_bool());
        });
        unsafe {
            AVCaptureDevice::requestAccessForMediaType_completionHandler(media_type, &completion);
        }
        if receiver
            .recv()
            .context("macOS microphone permission request failed")?
        {
            return Ok(());
        }
    }
    bail!("macOS microphone access is blocked. Enable the app hosting this terminal under System Settings > Privacy & Security > Microphone and restart it. If no permission dialog appears, the host app may lack the macOS audio-input entitlement; run this CLI from Ghostty or Terminal instead.");
}

#[cfg(not(target_os = "macos"))]
fn ensure_microphone_access() -> Result<()> {
    Ok(())
}

fn callback<T>(
    sender: mpsc::Sender<Vec<f32>>,
    errors: mpsc::UnboundedSender<String>,
    channels: usize,
    rate: u32,
) -> impl FnMut(&[T], &cpal::InputCallbackInfo) + Send + 'static
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    // Streaming resampler keeps phase across callbacks; 16 kHz mono for Whisper.
    let mut resampler = Resampler::new(rate);
    let mut reported_overflow = false;
    move |data, _| {
        let mono: Vec<f32> = data
            .chunks_exact(channels)
            .map(|frame| {
                frame
                    .iter()
                    .map(|s| <f32 as cpal::FromSample<T>>::from_sample_(*s))
                    .sum::<f32>()
                    / channels as f32
            })
            .collect();
        let samples = resampler.push(&mono);
        // Never block the real-time callback; make an overflow visible once per burst.
        if sender.try_send(samples).is_err() {
            if !reported_overflow {
                let _ = errors.send(
                    "Microphone buffer overflow: some audio was lost. Reduce system load.".into(),
                );
            }
            reported_overflow = true;
        } else {
            reported_overflow = false;
        }
    }
}

pub struct Resampler {
    step: f64,
    next: f64,
    offset: usize,
    previous: f32,
}
impl Resampler {
    pub fn new(rate: u32) -> Self {
        Self {
            step: rate as f64 / 16000.0,
            next: 0.0,
            offset: 0,
            previous: 0.0,
        }
    }
    pub fn push(&mut self, input: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        for (i, &sample) in input.iter().enumerate() {
            let index = (self.offset + i) as f64;
            while self.next <= index {
                let frac = (self.next - (index - 1.0)).clamp(0.0, 1.0) as f32;
                out.push(self.previous + (sample - self.previous) * frac);
                self.next += self.step;
            }
            self.previous = sample;
        }
        self.offset += input.len();
        out
    }
}

pub struct Segmenter {
    pre: VecDeque<f32>,
    samples: Vec<f32>,
    silence: usize,
    voiced: usize,
    settings: Settings,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
}
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Segment {
    pub samples: Vec<f32>,
    pub started_at: chrono::DateTime<chrono::Utc>,
}
impl Segmenter {
    pub fn is_active(&self) -> bool {
        self.started_at.is_some()
    }
    pub fn new(settings: Settings) -> Self {
        Self {
            pre: VecDeque::new(),
            samples: vec![],
            silence: 0,
            voiced: 0,
            settings,
            started_at: None,
        }
    }
    pub fn push(&mut self, samples: &[f32]) -> Option<Segment> {
        let rms = rms(samples);
        let speech = rms > self.settings.speech_threshold;
        if self.samples.is_empty() && !speech {
            self.pre.extend(samples);
            while self.pre.len() > 4800 {
                self.pre.pop_front();
            }
            return None;
        }
        if self.samples.is_empty() {
            self.started_at = Some(
                chrono::Utc::now()
                    - chrono::Duration::milliseconds(
                        ((self.pre.len() + samples.len()) / 16) as i64,
                    ),
            );
            self.samples.extend(self.pre.drain(..));
        }
        self.samples.extend_from_slice(samples);
        if speech {
            self.silence = 0;
            self.voiced += samples.len();
        } else {
            self.silence += samples.len();
        }
        if self.silence >= self.settings.silence_ms as usize * 16
            || self.samples.len() >= self.settings.max_segment_seconds as usize * 16000
        {
            return self.flush();
        }
        None
    }
    pub fn flush(&mut self) -> Option<Segment> {
        let samples = std::mem::take(&mut self.samples);
        let voiced = std::mem::replace(&mut self.voiced, 0);
        self.silence = 0;
        let started_at = self.started_at.take().unwrap_or_else(chrono::Utc::now);
        if voiced >= 4000 {
            Some(Segment {
                samples,
                started_at,
            })
        } else {
            None
        }
    }
}
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|v| v * v).sum::<f32>() / samples.len() as f32).sqrt()
}
pub fn write_wav(path: &std::path::Path, samples: &[f32]) -> Result<()> {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )?;
    for sample in samples {
        writer.write_sample((sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    writer.finalize()?;
    Ok(())
}
pub fn read_wav(path: &std::path::Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    if spec.channels == 0 || spec.sample_rate == 0 {
        bail!("Invalid WAV format");
    }
    let samples: Vec<f32> = if spec.sample_format == hound::SampleFormat::Float {
        reader
            .samples::<f32>()
            .collect::<std::result::Result<_, _>>()?
    } else {
        let scale = 2f32.powi(spec.bits_per_sample as i32 - 1);
        reader
            .samples::<i32>()
            .map(|s| s.map(|v| v as f32 / scale))
            .collect::<std::result::Result<_, _>>()?
    };
    let mono: Vec<f32> = samples
        .chunks_exact(spec.channels as usize)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect();
    Ok(Resampler::new(spec.sample_rate).push(&mono))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resampling_is_independent_of_callback_boundaries() {
        let input: Vec<_> = (0..44100).map(|i| (i as f32 * 0.03).sin()).collect();
        let expected = Resampler::new(44100).push(&input);
        let mut resampler = Resampler::new(44100);
        let actual: Vec<_> = input.chunks(137).flat_map(|c| resampler.push(c)).collect();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 16000);
    }
    #[test]
    fn silence_is_discarded_and_speech_flushed() {
        let mut segmenter = Segmenter::new(Settings::default());
        for _ in 0..100 {
            assert!(segmenter.push(&[0.0; 1600]).is_none());
        }
        for _ in 0..5 {
            assert!(segmenter.push(&[0.1; 1600]).is_none());
        }
        let mut segment = None;
        for _ in 0..14 {
            segment = segmenter.push(&[0.0; 1600]);
        }
        assert!(segment.unwrap().samples.len() <= 35200);
        assert!(segmenter.flush().is_none());
    }
}
