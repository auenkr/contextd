//! Real-model smoke check, with isolated application storage supplied via env.
use contextd_core::{audio, models, Service};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = Service::new()?;
    let settings = service.settings();
    println!(
        "Downloading default speech model: {}",
        settings.speech_model
    );
    models::download(&service.paths, &settings.speech_model, &service.events).await?;
    let response = reqwest::get(
        "https://raw.githubusercontent.com/ggml-org/whisper.cpp/master/samples/jfk.wav",
    )
    .await?
    .error_for_status()?
    .bytes()
    .await?;
    let sample = service.paths.root.join("sample.wav");
    std::fs::write(&sample, response)?;
    let entry = service
        .process(audio::read_wav(&sample)?, settings)
        .await?
        .expect("Expected transcript");
    println!("Transcribed: {}", entry.raw);
    assert!(entry.raw.to_lowercase().contains("country"));
    let day = chrono::DateTime::parse_from_rfc3339(&entry.created_at)?
        .with_timezone(&chrono::Local)
        .date_naive();
    let markdown = service.paths.root.join("notes").join(format!("{day}.md"));
    let contents = std::fs::read_to_string(&markdown)?;
    assert!(contents.contains(&entry.raw));
    assert!(contents.contains("## Around"));
    assert!(!service.history("country", 10)?.is_empty());
    service.unload().await?;
    println!(
        "Verified default Parakeet inference, daily Markdown, search, and unload: {}",
        markdown.display()
    );
    Ok(())
}
