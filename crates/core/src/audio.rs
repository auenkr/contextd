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
pub struct InputAudio {
    pub source: usize,
    pub samples: Vec<f32>,
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
        Self::start_with(
            microphone,
            move |samples| sender.try_send(samples).is_ok(),
            errors,
        )
    }
    pub fn start_tagged(
        microphone: Option<String>,
        source: usize,
        sender: mpsc::Sender<InputAudio>,
        errors: mpsc::UnboundedSender<String>,
    ) -> Result<Self> {
        Self::start_with(
            microphone,
            move |samples| sender.try_send(InputAudio { source, samples }).is_ok(),
            errors,
        )
    }
    fn start_with(
        microphone: Option<String>,
        deliver: impl FnMut(Vec<f32>) -> bool + Send + 'static,
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
                        callback::<f32>(deliver, errors.clone(), channels, rate),
                        err,
                        None,
                    )?,
                    cpal::SampleFormat::I16 => device.build_input_stream(
                        &config.clone().into(),
                        callback::<i16>(deliver, errors.clone(), channels, rate),
                        err,
                        None,
                    )?,
                    cpal::SampleFormat::U16 => device.build_input_stream(
                        &config.clone().into(),
                        callback::<u16>(deliver, errors.clone(), channels, rate),
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
    /// Device output is independent of microphone streams and uses its own resampler.
    #[cfg(target_os = "macos")]
    pub fn start_device_output(
        source: usize,
        sender: mpsc::Sender<InputAudio>,
        errors: mpsc::UnboundedSender<String>,
    ) -> Result<Self> {
        use screencapturekit::{prelude::*, stream::delegate_trait::ErrorHandler};
        use std::sync::Mutex;

        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = thread::spawn(move || {
            let result = (|| -> Result<()> {
                let content = SCShareableContent::get().context(
                    "Device audio requires macOS 13+ and Screen & System Audio Recording permission. Enable your terminal under System Settings > Privacy & Security > Screen & System Audio Recording, then restart recording",
                )?;
                let display = content
                    .displays()
                    .into_iter()
                    .next()
                    .context("No display available for device audio capture")?;
                let filter = SCContentFilter::create()
                    .with_display(&display)
                    .with_excluding_windows(&[])
                    .build()?;
                let config = SCStreamConfiguration::new()
                    .with_width(2)
                    .with_height(2)
                    .with_captures_audio(true)
                    .with_sample_rate(48_000)
                    .with_channel_count(2)
                    .with_excludes_current_process_audio(true);
                let delegate_errors = errors.clone();
                let delegate = ErrorHandler::new(move |error| {
                    let _ = delegate_errors.send(format!("Device audio stopped: {error}. Restart recording after restoring Screen & System Audio Recording access."));
                });
                let mut stream = SCStream::new_with_delegate(&filter, &config, delegate)?;
                let resampler = Mutex::new(Resampler::new(48_000));
                let reported_error = AtomicBool::new(false);
                let callback_errors = errors.clone();
                // Register audio only: no screen frames are delivered or saved.
                stream.add_output_handler(move |sample: CMSampleBuffer, _: SCStreamOutputType| {
                    let result = (|| -> Result<()> {
                        let format = sample.format_description().context("Missing device audio format")?;
                        if format.audio_sample_rate() != Some(48_000.0)
                            || !format.audio_is_float() || format.audio_bits_per_channel() != Some(32)
                            || format.audio_is_big_endian() {
                            bail!("Unsupported device audio format; expected 48 kHz little-endian float32");
                        }
                        let buffers = sample.audio_buffer_list().map_err(|code| anyhow::anyhow!("Cannot read device audio buffer: {code}"))?;
                        let channels: Vec<_> = (0..buffers.num_buffers()).filter_map(|i| buffers.get(i))
                            .map(|buffer| (buffer.number_channels as usize, buffer.data())).collect();
                        let mono = output_mono(&channels)?;
                        let samples = resampler.lock().map_err(|_| anyhow::anyhow!("Device audio resampler lock failed"))?.push(&mono);
                        if !samples.is_empty() && sender.try_send(InputAudio {source, samples}).is_err() {
                            bail!("Device audio callback overflow; input audio was dropped");
                        }
                        Ok(())
                    })();
                    if let Err(error) = result {
                        if !reported_error.swap(true, Ordering::Relaxed) {
                            let _ = callback_errors.send(error.to_string());
                        }
                    }
                }, SCStreamOutputType::Audio)?;
                stream.start_capture().context("Could not start device audio; check Screen & System Audio Recording access for your terminal")?;
                let _ = ready_tx.send(Ok(
                    "Device audio · system output (48000 Hz, 2 channels)".to_string()
                ));
                while !flag.load(Ordering::Relaxed) {
                    thread::sleep(std::time::Duration::from_millis(50));
                }
                if let Err(error) = stream.stop_capture() {
                    let _ = errors.send(format!("Stopping device audio: {error}"));
                }
                Ok(())
            })();
            if let Err(error) = result {
                let _ = ready_tx.send(Err(format!("{error:#}")));
            }
        });
        match ready_rx.recv()? {
            Ok(description) => Ok(Self {
                description,
                stop,
                thread: Some(thread),
            }),
            Err(error) => {
                let _ = thread.join();
                bail!("{error}")
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn start_device_output(
        _source: usize,
        _sender: mpsc::Sender<InputAudio>,
        _errors: mpsc::UnboundedSender<String>,
    ) -> Result<Self> {
        bail!("Device audio capture is supported on macOS 13+; turn off record_device_audio on this platform");
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

/// Downmix planar or interleaved float32 CoreMedia buffers without alignment casts.
#[cfg(any(target_os = "macos", test))]
fn output_mono(buffers: &[(usize, &[u8])]) -> Result<Vec<f32>> {
    let channels: usize = buffers.iter().map(|(channels, _)| channels).sum();
    if channels == 0 {
        bail!("Device audio contains no channels");
    }
    let mut mono: Option<Vec<f32>> = None;
    for &(count, bytes) in buffers {
        if count == 0 || bytes.len() % (count * 4) != 0 {
            bail!("Malformed device audio buffer");
        }
        let frames = bytes.len() / (count * 4);
        let output = mono.get_or_insert_with(|| vec![0.0; frames]);
        if output.len() != frames {
            bail!("Device audio channel lengths differ");
        }
        for (frame, target) in bytes.chunks_exact(count * 4).zip(output.iter_mut()) {
            for value in frame.as_chunks::<4>().0 {
                let value = f32::from_le_bytes(*value);
                if !value.is_finite() {
                    bail!("Device audio contains a non-finite sample");
                }
                *target += value / channels as f32;
            }
        }
    }
    Ok(mono.unwrap_or_default())
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
    mut deliver: impl FnMut(Vec<f32>) -> bool + Send + 'static,
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
        if !deliver(samples) {
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

struct SpeechDetector(silero_vad_crs::SileroVad);
// SAFETY: the native detector owns its model and all recurrent/scratch buffers.
// Embedded weights are immutable. Moving ownership between threads is safe;
// calls require &mut self and the detector is never shared concurrently.
unsafe impl Send for SpeechDetector {}

pub struct Segmenter {
    detector: SpeechDetector,
    pending: VecDeque<f32>,
    pre: VecDeque<f32>,
    samples: Vec<f32>,
    silence: usize,
    voiced: usize,
    settings: Settings,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    stream_started_at: Option<chrono::DateTime<chrono::Utc>>,
    processed_samples: usize,
    onset_samples: usize,
    last_probability: f32,
}
const VAD_FRAME_SAMPLES: usize = 512; // 32 ms at 16 kHz.
const SPEECH_ONSET_SAMPLES: usize = 4 * VAD_FRAME_SAMPLES; // 128 ms of consecutive speech.
const MIN_SPEECH_SAMPLES: usize = 4000; // Keep the existing 250 ms minimum.
const PRE_ROLL_SAMPLES: usize = 4800; // Preserve word onsets during confirmation.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Segment {
    pub samples: Vec<f32>,
    pub started_at: chrono::DateTime<chrono::Utc>,
}
impl Segmenter {
    pub fn is_active(&self) -> bool {
        self.started_at.is_some()
    }
    pub fn last_speech_probability(&self) -> f32 {
        self.last_probability
    }
    pub fn new(settings: Settings) -> Result<Self> {
        Ok(Self {
            detector: SpeechDetector(
                silero_vad_crs::SileroVad::new()
                    .context("Could not initialize embedded speech detector")?,
            ),
            pending: VecDeque::new(),
            pre: VecDeque::new(),
            samples: vec![],
            silence: 0,
            voiced: 0,
            settings,
            started_at: None,
            stream_started_at: None,
            processed_samples: 0,
            onset_samples: 0,
            last_probability: 0.0,
        })
    }
    pub fn push(&mut self, samples: &[f32]) -> Result<Vec<Segment>> {
        if self.stream_started_at.is_none() && !samples.is_empty() {
            self.stream_started_at = Some(
                chrono::Utc::now()
                    - chrono::Duration::microseconds((samples.len() as i64 * 1000) / 16),
            );
        }
        self.pending.extend(samples);
        let mut segments = Vec::new();
        while self.pending.len() >= VAD_FRAME_SAMPLES {
            let mut frame = [0.0; VAD_FRAME_SAMPLES];
            for sample in &mut frame {
                *sample = self.pending.pop_front().unwrap();
            }
            if let Some(segment) = self.process_frame(&frame, frame.len())? {
                segments.push(segment);
            }
        }
        Ok(segments)
    }
    fn process_frame(
        &mut self,
        frame: &[f32; VAD_FRAME_SAMPLES],
        actual_samples: usize,
    ) -> Result<Option<Segment>> {
        // Always advance the recurrent detector, including during silence, so
        // its background-noise context does not become stale.
        self.last_probability = self.detector.0.forward_chunk(frame)?;
        let threshold = if self.is_active() {
            (self.settings.vad_threshold - 0.15).max(0.05)
        } else {
            self.settings.vad_threshold
        };
        let samples = &frame[..actual_samples];
        let speech =
            self.last_probability >= threshold && rms(samples) > self.settings.speech_threshold;
        Ok(self.push_classified_frame(samples, speech))
    }
    fn push_classified_frame(&mut self, samples: &[f32], speech: bool) -> Option<Segment> {
        self.processed_samples += samples.len();
        if self.samples.is_empty() {
            self.pre.extend(samples);
            while self.pre.len() > PRE_ROLL_SAMPLES {
                self.pre.pop_front();
            }
            self.onset_samples = if speech {
                self.onset_samples + samples.len()
            } else {
                0
            };
            if self.onset_samples < SPEECH_ONSET_SAMPLES {
                return None;
            }
            self.started_at = Some(
                self.stream_started_at.unwrap_or_else(chrono::Utc::now)
                    + chrono::Duration::microseconds(
                        ((self.processed_samples - self.pre.len()) as i64 * 1000) / 16,
                    ),
            );
            self.samples.extend(self.pre.drain(..));
            self.voiced = self.onset_samples;
            self.onset_samples = 0;
            return None;
        }
        self.samples.extend_from_slice(samples);
        if speech {
            self.silence = 0;
            self.voiced += samples.len();
        } else {
            self.silence += samples.len();
        }
        if self.silence >= self.settings.silence_ms as usize * 16 {
            return self.finish_segment();
        }
        None
    }
    pub fn flush(&mut self) -> Result<Option<Segment>> {
        if !self.pending.is_empty() {
            let actual_samples = self.pending.len();
            let mut frame = [0.0; VAD_FRAME_SAMPLES];
            for sample in &mut frame[..actual_samples] {
                *sample = self.pending.pop_front().unwrap();
            }
            if let Some(segment) = self.process_frame(&frame, actual_samples)? {
                return Ok(Some(segment));
            }
        }
        Ok(self.finish_segment())
    }
    fn finish_segment(&mut self) -> Option<Segment> {
        let samples = std::mem::take(&mut self.samples);
        let voiced = std::mem::replace(&mut self.voiced, 0);
        self.silence = 0;
        self.onset_samples = 0;
        let started_at = self.started_at.take().unwrap_or_else(chrono::Utc::now);
        if voiced >= MIN_SPEECH_SAMPLES {
            Some(Segment {
                samples,
                started_at,
            })
        } else {
            None
        }
    }
}

/// Screen imported audio and old queued jobs before loading a transcription
/// model. Stop as soon as a minimum-duration speech segment is confirmed.
pub fn has_speech(samples: &[f32], settings: &Settings) -> Result<bool> {
    let mut segmenter = Segmenter::new(settings.clone())?;
    for frame in samples.chunks(VAD_FRAME_SAMPLES) {
        if !segmenter.push(frame)?.is_empty() || segmenter.voiced >= MIN_SPEECH_SAMPLES {
            return Ok(true);
        }
    }
    Ok(segmenter.flush()?.is_some())
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
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "plays generated speech and requires macOS system audio recording access"]
    fn live_device_output_delivers_tagged_speech() {
        let directory = tempfile::tempdir().unwrap();
        let speech = directory.path().join("device-test.aiff");
        assert!(std::process::Command::new("say")
            .arg("-o")
            .arg(&speech)
            .arg("This is a device audio capture test. The meeting is tomorrow morning.")
            .status()
            .unwrap()
            .success());
        let (sender, mut receiver) = mpsc::channel(256);
        let (errors, mut error_rx) = mpsc::unbounded_channel();
        let mut capture = Capture::start_device_output(7, sender, errors).unwrap();
        let mut player = std::process::Command::new("afplay")
            .arg(&speech)
            .spawn()
            .unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (samples, peak) = runtime.block_on(async {
            let deadline = tokio::time::sleep(std::time::Duration::from_secs(8));
            tokio::pin!(deadline);
            let mut samples = Vec::new(); let mut peak = 0.0f32;
            loop { tokio::select! {
                _ = &mut deadline => break,
                Some(audio) = receiver.recv() => {
                    assert_eq!(audio.source, 7); peak = peak.max(rms(&audio.samples)); samples.extend(audio.samples);
                }
            }}
            (samples, peak)
        });
        let _ = player.wait();
        capture.stop();
        assert!(
            error_rx.try_recv().is_err(),
            "device audio reported an error"
        );
        assert!(
            samples.len() > 16_000 && peak > 0.001,
            "No audible device output: {} samples, peak RMS {peak}",
            samples.len()
        );
        assert!(has_speech(&samples, &Settings::default()).unwrap());
        println!(
            "Device output: {} tagged 16 kHz samples, peak RMS {peak:.4}",
            samples.len()
        );
        if let Some(cache) = std::env::var_os("CONTEXTD_TEST_MODEL_CACHE") {
            use crate::inference::SpeechToText;
            let mut paths = crate::Paths::in_directory(directory.path());
            paths.cache_dir = cache.into();
            let mut engine = crate::inference::LocalSpeechEngine::new(paths);
            let settings = Settings {
                speech_model: std::env::var("CONTEXTD_TEST_SPEECH_MODEL")
                    .unwrap_or_else(|_| Settings::default().speech_model),
                language: "en".into(),
                ..Settings::default()
            };
            let transcript = engine.transcribe(&samples, &settings).unwrap();
            let lower = transcript.to_lowercase();
            assert!(
                lower.contains("meeting") && lower.contains("tomorrow"),
                "{transcript}"
            );
            println!("Device output transcript: {transcript}");
        }
    }

    #[test]
    fn device_audio_downmix_accepts_planar_and_interleaved_stereo() {
        let left: Vec<u8> = [1.0f32, 0.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        let right: Vec<u8> = [-1.0f32, 0.5]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        let packed: Vec<u8> = [1.0f32, -1.0, 0.0, 0.5]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        assert_eq!(
            output_mono(&[(1, &left), (1, &right)]).unwrap(),
            vec![0.0, 0.25]
        );
        assert_eq!(output_mono(&[(2, &packed)]).unwrap(), vec![0.0, 0.25]);
        assert!(output_mono(&[(0, &left)]).is_err());
        assert!(output_mono(&[(2, &packed[..3])]).is_err());
        assert!(output_mono(&[(1, &left), (1, &right[..4])]).is_err());
        assert!(output_mono(&[(1, &f32::NAN.to_le_bytes())]).is_err());
    }

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
        let mut segmenter = Segmenter::new(Settings::default()).unwrap();
        for _ in 0..100 {
            assert!(segmenter.push(&[0.0; 1600]).unwrap().is_empty());
        }
        // Test segmentation independently of classification; a constant DC
        // signal is not human speech and must not be used as a VAD fixture.
        for _ in 0..5 {
            assert!(segmenter
                .push_classified_frame(&[0.1; 1600], true)
                .is_none());
        }
        let mut segment = None;
        for _ in 0..14 {
            segment = segmenter.push_classified_frame(&[0.0; 1600], false);
        }
        assert!(segment.unwrap().samples.len() <= 35200);
        assert!(segmenter.flush().unwrap().is_none());
    }

    #[test]
    fn continuous_confirmed_speech_stays_in_one_segment_until_a_pause() {
        let mut segmenter = Segmenter::new(Settings::default()).unwrap();
        for _ in 0..650 {
            assert!(segmenter
                .push_classified_frame(&[0.1; 1600], true)
                .is_none());
        }
        assert!(segmenter.is_active());
        for _ in 0..13 {
            assert!(segmenter
                .push_classified_frame(&[0.0; 1600], false)
                .is_none());
        }
        let segment = segmenter
            .push_classified_frame(&[0.0; 1600], false)
            .unwrap();
        assert_eq!(segment.samples.len(), 664 * 1600);
        assert!(!segmenter.is_active());
    }

    #[test]
    fn short_detection_spikes_do_not_start_capture_and_pre_roll_keeps_word_onsets() {
        let mut segmenter = Segmenter::new(Settings::default()).unwrap();
        for _ in 0..12 {
            segmenter.push_classified_frame(&[0.01; VAD_FRAME_SAMPLES], false);
        }
        for _ in 0..3 {
            segmenter.push_classified_frame(&[0.1; VAD_FRAME_SAMPLES], true);
            assert!(!segmenter.is_active());
        }
        segmenter.push_classified_frame(&[0.01; VAD_FRAME_SAMPLES], false);
        assert!(!segmenter.is_active());
        assert!(segmenter.flush().unwrap().is_none());
        for _ in 0..8 {
            segmenter.push_classified_frame(&[0.1; VAD_FRAME_SAMPLES], true);
        }
        let segment = segmenter.flush().unwrap().unwrap();
        assert_eq!(
            segment.samples.len(),
            PRE_ROLL_SAMPLES + 4 * VAD_FRAME_SAMPLES
        );
        assert!(segment.samples[PRE_ROLL_SAMPLES - SPEECH_ONSET_SAMPLES..]
            .iter()
            .all(|sample| *sample == 0.1));
    }

    #[test]
    fn loud_fan_like_noise_and_dc_do_not_start_or_queue_speech() {
        let mut state = 17u32;
        let mut lowpass = 0.0;
        let noise: Vec<f32> = (0..8 * 16000)
            .map(|i| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                let white = state as f32 / u32::MAX as f32 * 2.0 - 1.0;
                lowpass = 0.9 * lowpass + 0.1 * white;
                let t = i as f32 / 16000.0;
                let phase = 2.0 * std::f32::consts::PI * t;
                (0.25 * lowpass + 0.035 * (100.0 * phase).sin() + 0.015 * (200.0 * phase).sin())
                    * (1.0 + 0.4 * (0.27 * phase).sin())
            })
            .collect();
        assert!(rms(&noise) > Settings::default().speech_threshold);
        for samples in [&noise[..], &vec![0.1; 16000][..]] {
            let mut segmenter = Segmenter::new(Settings::default()).unwrap();
            for chunk in samples.chunks(193) {
                assert!(segmenter.push(chunk).unwrap().is_empty());
                assert!(!segmenter.is_active());
            }
            assert!(segmenter.flush().unwrap().is_none());
            assert!(!has_speech(samples, &Settings::default()).unwrap());
        }
    }
}
