//! Apply the configured policy without a word or phrase blacklist.
use crate::config::ShortTranscriptPolicy;

pub fn should_skip(text: &str, policy: ShortTranscriptPolicy) -> bool {
    match policy {
        ShortTranscriptPolicy::Keep => false,
        ShortTranscriptPolicy::Cleanup => text.trim().is_empty(),
        ShortTranscriptPolicy::SkipAll => {
            text.split(|character: char| !character.is_alphanumeric() && character != '\'')
                .filter(|word| !word.is_empty())
                .take(4)
                .count()
                <= 3
        }
    }
}
