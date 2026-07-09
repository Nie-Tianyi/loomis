//! Convenience: DeepSeek-backed memory compaction.

use deepseek::DeepSeekClient;
use memory::{Memory, MemoryError};
use provider::Message;
use provider::Role;

/// Compact `memory` using a DeepSeek flash model for summarisation.
///
/// Three-phase process:
/// 1. [`Memory::drain_for_compact`] — extract old non-System messages
/// 2. Send to model for summarisation
/// 3. [`Memory::apply_compact`] — insert summary at position 0
pub async fn compact_with_deepseek(
    memory: &mut Memory,
    client: &DeepSeekClient,
    model: &str,
) -> Result<(), MemoryError> {
    let old = memory.drain_for_compact();
    if old.is_empty() {
        return Err(MemoryError::NothingToCompact);
    }

    let transcript: String = old
        .iter()
        .map(|m| format!("[{}]: {}", role_label(m.role), m.content))
        .collect::<Vec<_>>()
        .join("\n\n");

    let prompt = format!(
        "Summarise the following conversation history concisely. \
         Preserve key facts, decisions, and context. \
         Output only the summary, no preamble:\n\n{transcript}"
    );

    let request = deepseek::DeepSeekRequest::new(model, vec![Message::new(Role::User, prompt)]);

    let response = client
        .send(request)
        .await
        .map_err(|e| MemoryError::SummariserFailed(format!("LLM call failed: {e}")))?;

    let summary = response
        .choices
        .first()
        .and_then(|c| c.message.content.clone())
        .unwrap_or_default();

    memory.apply_compact(summary);
    Ok(())
}

const fn role_label(role: Role) -> &'static str {
    match role {
        Role::System => "System",
        Role::User => "User",
        Role::Assistant => "Assistant",
        Role::Tool => "Tool",
    }
}
