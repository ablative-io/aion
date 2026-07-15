//! Deterministic output clipping shared by shell activity fields.

/// Maximum number of source characters retained before a truncation marker is inserted.
pub const CLIP_LIMIT_CHARS: usize = 16_384;

/// Clip long text while preserving both its beginning and end.
///
/// Short text is returned unchanged. Long text retains one quarter of the
/// character budget from the head and the remainder from the tail, separated
/// by an explicit marker naming the number of omitted characters.
#[must_use]
pub fn clip_text(text: &str) -> String {
    let character_count = text.chars().count();
    if character_count <= CLIP_LIMIT_CHARS {
        return text.to_owned();
    }

    let head_count = CLIP_LIMIT_CHARS / 4;
    let tail_count = CLIP_LIMIT_CHARS - head_count;
    let omitted = character_count - CLIP_LIMIT_CHARS;
    let head: String = text.chars().take(head_count).collect();
    let tail: String = text.chars().skip(character_count - tail_count).collect();

    format!("{head}\n--- output truncated: {omitted} characters omitted ---\n{tail}")
}
