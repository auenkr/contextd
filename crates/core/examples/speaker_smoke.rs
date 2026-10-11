//! Real local diarization, cross-run matching, renaming, and transcription smoke test.
use contextd_core::{audio, history::History, models, speakers, Service};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = Service::new()?;
    let paths = service.paths.clone();
    let settings = service.settings();
    let filename = paths.root.join("four-speakers.wav");
    if !filename.exists() {
        let response = reqwest::get("https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/0-four-speakers-zh.wav").await?.error_for_status()?.bytes().await?;
        std::fs::write(&filename, response)?;
    }
    let samples = audio::read_wav(&filename)?;
    let mut engine = speakers::Engine::default();
    let first = engine.detect(&paths, &settings, &samples)?;
    anyhow::ensure!(
        first.voices.len() >= 2,
        "Expected multiple clearly separated voices"
    );
    let mut identities = std::collections::HashMap::new();
    for voice in &first.voices {
        let mut clip = Vec::new();
        for range in &voice.ranges {
            let start = (range[0] * 16000.0) as usize;
            let end = ((range[1] * 16000.0) as usize).min(samples.len());
            if start < end {
                clip.extend_from_slice(&samples[start..end]);
            }
        }
        let speaker = speakers::identify(&paths, &voice.embedding, &clip, &settings)?
            .expect("New distinct voice");
        assert!(std::path::Path::new(&speaker.sample_path).exists());
        assert!(speaker.sample_seconds >= 2.0 && speaker.sample_seconds <= 15.0);
        identities.insert(voice.local_speaker, speaker.id);
    }
    let count = speakers::list(&paths)?.len();
    anyhow::ensure!(
        count == first.voices.len(),
        "Distinct diarized voices must have distinct profiles"
    );
    let id = identities.values().next().unwrap().clone();
    speakers::rename(&paths, &settings, &id, "Test speaker")?;
    engine.unload();
    let second = engine.detect(&paths, &settings, &samples)?;
    for voice in &second.voices {
        let speaker = speakers::identify(&paths, &voice.embedding, &samples, &settings)?
            .expect("Known voice after restart");
        assert_eq!(speaker.id, identities[&voice.local_speaker]);
        if speaker.id == id {
            assert_eq!(speaker.name, "Test speaker");
        }
    }
    assert_eq!(speakers::list(&paths)?.len(), count);
    // Test whole application path with a public single-speaker English recording.
    models::download(&paths, &settings.speech_model, &service.events).await?;
    let sample = paths.root.join("jfk.wav");
    if !sample.exists() {
        let response = reqwest::get(
            "https://raw.githubusercontent.com/ggml-org/whisper.cpp/master/samples/jfk.wav",
        )
        .await?
        .error_for_status()?
        .bytes()
        .await?;
        std::fs::write(&sample, response)?;
    }
    let entry = service
        .process(audio::read_wav(&sample)?, settings.clone())
        .await?
        .expect("Speech transcript");
    assert!(entry.speaker_error.is_none(), "{:?}", entry.speaker_error);
    assert!(!entry.speaker_turns.is_empty());
    assert!(entry.raw.to_lowercase().contains("country"));
    let turn = entry
        .speaker_turns
        .iter()
        .find(|turn| turn.speaker_id.is_some())
        .expect("Enroll JFK voice");
    let id = turn.speaker_id.clone().unwrap();
    speakers::rename(&paths, &settings, &id, "JFK")?;
    let latest = History::open(&paths)?.search("JFK", 10)?;
    assert!(latest.iter().any(|note| note.id == entry.id));
    let file = contextd_core::history::note_path(&paths, &entry.created_at)?;
    assert!(std::fs::read_to_string(file)?.contains("JFK"));
    assert!(!paths
        .root
        .join("audio")
        .join(format!("{}.wav", entry.id))
        .exists());
    println!("PASS: {} distinct profiles, saved samples, repeat voice matching after restart, naming, real Parakeet transcription, speaker attribution and name search with full-audio retention off. Data: {}",count,paths.root.display());
    service.unload().await?;
    Ok(())
}
