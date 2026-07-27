//! Stage 13 — extract a task's **intent**: what it was trying to achieve.
//!
//! The existing embeddings answer different questions. The content embedding
//! covers the whole raw sample blob — mostly plumbing, timestamps and tool
//! output. The behavioral embedding is a 36-dimensional structural fingerprint,
//! which clusters by *shape* rather than by meaning. Neither answers "what was
//! this task for", which is the key a semantic search over tasks needs.
//!
//! This module picks the sentence that states the goal, so it can be embedded on
//! its own. Embedding the goal rather than the transcript is what makes
//! "find tasks like this one" return tasks with a similar *purpose* instead of
//! tasks with a similar amount of logging.
//!
//! # Selection order
//!
//! Most specific statement of intent first — see [`extract`]. When nothing
//! qualifies the function returns `None` and no embedding is produced, which is
//! deliberate: embedding an arbitrary log line would put noise into the index and
//! quietly degrade every future search against it.

use crate::models::{EntityRecord, SemanticRole};

/// Longest intent text to keep, in characters.
///
/// A goal statement is a sentence or two. Anything much longer is a transcript
/// that happened to land in a prompt field, and embedding it would drown the
/// actual intent in context — the exact failure the content embedding already has.
pub const MAX_INTENT_CHARS: usize = 2_000;

/// Shortest text worth embedding.
///
/// `"ok"` or `"go"` carries no retrievable meaning; indexing it would produce a
/// vector that matches everything weakly and nothing well.
pub const MIN_INTENT_CHARS: usize = 12;

/// Extract the task's goal statement from its entities.
///
/// Tries each source in turn and takes the first usable one:
///
/// 1. **System prompt** — states the agent's purpose directly, and is the most
///    stable across runs of the same task type.
/// 2. **User turn** — what was actually asked. More specific than the system
///    prompt but more variable, so it ranks second.
/// 3. **Agent reasoning** — the agent's own first `Thought:`. A restatement of
///    the goal in the agent's words; a decent last resort.
///
/// Returns `None` when none of those exist or all are too short — in which case
/// the task simply has no intent embedding, rather than a misleading one.
pub fn extract(entities: &[EntityRecord]) -> Option<String> {
    const ORDER: &[SemanticRole] = &[
        SemanticRole::SystemPrompt,
        SemanticRole::UserTurn,
        SemanticRole::AgentReasoning,
    ];

    for role in ORDER {
        // Scan by role across all entities, not entity-by-entity, so a user turn
        // on line 1 cannot beat a system prompt on line 4.
        if let Some(text) = entities
            .iter()
            .filter(|e| &e.semantic_role == role)
            .filter_map(|e| usable_text(e))
            .filter_map(|t| qualify(role, &t))
            .next()
        {
            return Some(text);
        }
    }
    None
}

/// Apply the per-role quality bar, returning the text to embed.
///
/// `AgentReasoning` needs a stricter test than the other two. The semantic
/// classifier assigns that role to anything matching its `AgentStep` patterns,
/// which includes lifecycle lines — `AgentExecutor running`, `crew.kickoff
/// called`, `crew.kickoff finished`. Those are not goals, and embedding one would
/// make every run of that framework look identical, which defeats the search
/// outright. Observed on the langchain and crewai fixtures, where the first
/// `agent_reasoning` entity is exactly such a line.
///
/// So a reasoning entity only qualifies when it carries an explicit reasoning
/// marker, and the marker is stripped so the vector covers the thought rather
/// than the prefix.
fn qualify(role: &SemanticRole, text: &str) -> Option<String> {
    if role != &SemanticRole::AgentReasoning {
        return (!is_lifecycle_noise(text)).then(|| text.to_string());
    }

    // `Final Answer:` is the conclusion, not the goal — deliberately absent.
    const REASONING_MARKERS: &[&str] = &["thought:", "reasoning:", "plan:", "goal:", "task:"];

    let lowered = text.to_ascii_lowercase();
    let marker = REASONING_MARKERS
        .iter()
        .find(|m| lowered.starts_with(*m))?;

    let stripped = text[marker.len()..].trim();
    normalise(stripped)
}

/// Reject text that describes the run's machinery rather than its purpose.
fn is_lifecycle_noise(text: &str) -> bool {
    const NOISE: &[&str] = &[
        "agentexecutor",
        "crew.kickoff",
        "agent initialised",
        "agent initialized",
        "session started",
        "session ended",
        "run started",
        "run finished",
    ];
    let lowered = text.to_ascii_lowercase();
    NOISE.iter().any(|n| lowered.contains(n))
}

/// Pull the most goal-like text out of one entity.
///
/// Prefers a structured `content` / `prompt` / `input` field over `raw_text`,
/// because `raw_text` is the whole JSON line — braces, timestamps, token counts
/// — and embedding that buries the sentence that matters.
///
/// # The `raw_text` fallback is only valid for plain-text logs
///
/// `extracted_fields` holds a line's **top-level** keys and is not flattened, so
/// the ordinary shape of an LLM log —
/// `{"message":{"content":[{"type":"text","text":"Summarise Q3 revenue"}]}}` —
/// matches no field: `message` exists but is an object, so `as_str()` is `None`.
/// Falling through to `raw_text` there made the task's stated goal the entire
/// JSON line. It cleared `MIN_INTENT_CHARS`, so nothing rejected it; it was
/// written by `set_task_intent_if_absent`, where the first writer wins
/// permanently; and it was then embedded and indexed. That is exactly what the
/// paragraph above says must not happen, bypassed by the common case.
///
/// So the fallback now applies only when the line is *not* structured. A JSON
/// line whose text is nested yields `None` — no intent is better than an intent
/// made of token counts, because a wrong one is permanent and poisons search.
fn usable_text(entity: &EntityRecord) -> Option<String> {
    const TEXT_FIELDS: &[&str] = &["content", "prompt", "input", "message", "msg", "text", "query"];

    let field_text = TEXT_FIELDS.iter().find_map(|k| {
        entity
            .extracted_fields
            .get(*k)
            .and_then(|v| v.as_str())
            .map(str::to_string)
    });

    let candidate = match field_text {
        Some(text) => text,
        // Plain-text logs have no fields at all; the line itself is the content.
        None if !looks_structured(&entity.raw_text) => entity.raw_text.clone(),
        None => return None,
    };

    normalise(&candidate)
}

/// Whether a line is structured, and so whose `raw_text` is machine syntax
/// rather than prose.
///
/// Deliberately cheap and shape-based rather than a parse: the question is only
/// "would embedding this line embed punctuation instead of a sentence", and a
/// line that opens with a brace or bracket answers yes whether or not it is
/// well-formed JSON.
fn looks_structured(raw: &str) -> bool {
    let trimmed = raw.trim_start();
    trimmed.starts_with('{') || trimmed.starts_with('[')
}

/// Collapse whitespace, trim, and reject text too short or too long to be useful.
fn normalise(text: &str) -> Option<String> {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() < MIN_INTENT_CHARS {
        return None;
    }
    if collapsed.chars().count() > MAX_INTENT_CHARS {
        // Truncate on a char boundary, not a byte one.
        return Some(collapsed.chars().take(MAX_INTENT_CHARS).collect());
    }
    Some(collapsed)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{EntityType, SemanticRole};
    use std::collections::HashMap;

    fn entity(role: SemanticRole, content: &str) -> EntityRecord {
        let mut fields = HashMap::new();
        fields.insert("content".to_string(), serde_json::json!(content));
        EntityRecord {
            entity_id: "e".to_string(),
            entity_type: EntityType::PromptEvent,
            semantic_role: role,
            sample_hash: "h".to_string(),
            target_id: "t".to_string(),
            trace_id: "tr".to_string(),
            span_id: "sp".to_string(),
            parent_span_id: None,
            prov_entity_id: "ug:entity:e".to_string(),
            prov_activity_id: "ug:activity:h:0".to_string(),
            line_index: 0,
            raw_text: String::new(),
            extracted_fields: fields,
            model_id: None,
            tool_name: None,
            mcp_server_id: None,
            token_count: None,
            latency_ms: None,
            timestamp_utc: None,
            content_embedding_id: None,
            behavioral_embedding_id: None,
            task_id: String::new(),
            correlation_key: None,
        }
    }

    fn raw_entity(role: SemanticRole, raw: &str) -> EntityRecord {
        let mut e = entity(role, "");
        e.extracted_fields.clear();
        e.raw_text = raw.to_string();
        e
    }

    // ── A structured line is never its own intent ────────────────────────────

    #[test]
    fn a_json_line_whose_text_is_nested_yields_no_intent() {
        // `extracted_fields` holds top-level keys only and is not flattened, so
        // this shape — the ordinary one for an LLM log — matched no text field:
        // `message` exists but is an object, so `as_str()` is None. Falling
        // through to `raw_text` made the task's stated goal the whole JSON line,
        // token counts and all. It cleared MIN_INTENT_CHARS, was written by
        // `set_task_intent_if_absent` where the first writer wins permanently,
        // and was then embedded.
        let raw = r#"{"ts":"2026-04-26T10:00:00Z","model":"claude-opus-4","#.to_string()
            + r#""message":{"content":[{"type":"text","text":"Summarise Q3 revenue"}]},"#
            + r#""usage":{"input_tokens":812,"output_tokens":44}}"#;

        let e = raw_entity(SemanticRole::UserTurn, &raw);
        assert_eq!(
            extract(&[e]),
            None,
            "no intent is better than an intent made of machine syntax",
        );
    }

    #[test]
    fn a_plain_text_line_is_still_usable_as_its_own_intent() {
        // The fallback is correct for react/bedrock-style logs, where the line
        // genuinely is the prose. Only structured lines lose it.
        let e = raw_entity(SemanticRole::UserTurn, "Find the cheapest flight to Lisbon");
        assert_eq!(
            extract(&[e]).as_deref(),
            Some("Find the cheapest flight to Lisbon"),
        );
    }

    #[test]
    fn a_structured_line_with_a_top_level_text_field_still_works() {
        // The guard is on the *fallback*, not on JSON as such: when the field is
        // there at the top level, it is used as before.
        let mut e = raw_entity(SemanticRole::UserTurn, r#"{"content":"Summarise Q3 revenue"}"#);
        e.extracted_fields
            .insert("content".to_string(), serde_json::json!("Summarise Q3 revenue"));
        assert_eq!(extract(&[e]).as_deref(), Some("Summarise Q3 revenue"));
    }

    #[test]
    fn looks_structured_is_not_fooled_by_leading_whitespace() {
        assert!(looks_structured("   {\"a\":1}"));
        assert!(looks_structured("[{\"a\":1}]"));
        assert!(!looks_structured("Summarise the Q3 report {see attached}"));
    }

    // ── Selection order ──────────────────────────────────────────────────────

    #[test]
    fn a_system_prompt_wins() {
        let out = extract(&[
            entity(SemanticRole::UserTurn, "Find the weather in Paris please"),
            entity(SemanticRole::SystemPrompt, "You are a helpful research assistant"),
        ]);
        assert_eq!(out.as_deref(), Some("You are a helpful research assistant"));
    }

    #[test]
    fn precedence_holds_across_lines_not_just_within_one() {
        // The regression guard: a lower-ranked role appearing earlier must not win.
        let out = extract(&[
            entity(SemanticRole::AgentReasoning, "I should search the web for this"),
            entity(SemanticRole::UserTurn, "Summarise the AI safety news"),
        ]);
        assert_eq!(out.as_deref(), Some("Summarise the AI safety news"));
    }

    #[test]
    fn a_user_turn_is_used_when_there_is_no_system_prompt() {
        let out = extract(&[entity(SemanticRole::UserTurn, "Write a blog post about Rust")]);
        assert_eq!(out.as_deref(), Some("Write a blog post about Rust"));
    }

    #[test]
    fn agent_reasoning_is_the_last_resort() {
        let out = extract(&[
            entity(SemanticRole::ToolInvocation, "calling tool web_search now"),
            entity(SemanticRole::AgentReasoning, "Thought: I need to find the current weather"),
        ]);
        assert_eq!(out.as_deref(), Some("I need to find the current weather"));
    }

    // ── The reasoning quality bar ────────────────────────────────────────────

    #[test]
    fn lifecycle_lines_are_not_intents() {
        // Observed on the real langchain and crewai fixtures: the first
        // `agent_reasoning` entity is a lifecycle line, and embedding it would
        // make every run of that framework look identical.
        for noise in [
            "AgentExecutor running",
            "crew.kickoff called",
            "crew.kickoff finished",
            "Agent initialised",
        ] {
            assert_eq!(
                extract(&[entity(SemanticRole::AgentReasoning, noise)]),
                None,
                "{noise:?} must not become a task intent",
            );
        }
    }

    #[test]
    fn reasoning_needs_an_explicit_marker() {
        // Without one it is indistinguishable from a status line.
        assert_eq!(
            extract(&[entity(SemanticRole::AgentReasoning, "some unmarked narration here")]),
            None,
        );
    }

    #[test]
    fn every_reasoning_marker_is_recognised_and_stripped() {
        for marker in ["Thought:", "Reasoning:", "Plan:", "Goal:", "Task:"] {
            let text = format!("{marker} Summarise the quarterly earnings report");
            let out = extract(&[entity(SemanticRole::AgentReasoning, &text)]);
            assert_eq!(
                out.as_deref(),
                Some("Summarise the quarterly earnings report"),
                "{marker} must be recognised and stripped",
            );
        }
    }

    #[test]
    fn a_final_answer_is_not_a_goal() {
        // It is the conclusion; embedding it would index the outcome as the intent.
        assert_eq!(
            extract(&[entity(
                SemanticRole::AgentReasoning,
                "Final Answer: The weather in San Francisco is partly cloudy",
            )]),
            None,
        );
    }

    #[test]
    fn a_lifecycle_line_falls_through_to_real_reasoning() {
        let out = extract(&[
            entity(SemanticRole::AgentReasoning, "AgentExecutor running"),
            entity(SemanticRole::AgentReasoning, "Thought: I should look up the forecast"),
        ]);
        assert_eq!(out.as_deref(), Some("I should look up the forecast"));
    }

    #[test]
    fn lifecycle_noise_is_rejected_for_prompts_too() {
        assert_eq!(
            extract(&[entity(SemanticRole::SystemPrompt, "AgentExecutor initialized tools=[a,b]")]),
            None,
        );
    }

    #[test]
    fn a_stripped_marker_leaving_too_little_is_rejected() {
        assert_eq!(extract(&[entity(SemanticRole::AgentReasoning, "Thought: ok")]), None);
    }

    #[test]
    fn roles_that_are_not_goal_statements_are_ignored() {
        // Tool output and completions describe what happened, not what was wanted.
        let out = extract(&[
            entity(SemanticRole::ToolResponse, "results: 10 documents retrieved"),
            entity(SemanticRole::AssistantTurn, "Here is the summary you asked for"),
        ]);
        assert_eq!(out, None);
    }

    // ── Text selection within an entity ──────────────────────────────────────

    #[test]
    fn a_structured_field_beats_raw_text() {
        // raw_text is the whole JSON line; embedding it buries the sentence.
        let mut e = entity(SemanticRole::SystemPrompt, "You are a research assistant");
        e.raw_text = r#"{"ts":"2026-01-01","role":"system","content":"You are a research assistant","tokens":42}"#.to_string();
        assert_eq!(extract(&[e]).as_deref(), Some("You are a research assistant"));
    }

    #[test]
    fn raw_text_is_the_fallback_for_plain_logs() {
        // Plain-text logs have no structured fields, so the line itself is the
        // content — and the `Thought:` marker is stripped so the vector covers
        // the thought rather than the prefix every ReAct line shares.
        let out = extract(&[raw_entity(
            SemanticRole::AgentReasoning,
            "Thought: I need to find the current weather in NYC",
        )]);
        assert_eq!(out.as_deref(), Some("I need to find the current weather in NYC"));
    }

    #[test]
    fn every_text_field_spelling_is_recognised() {
        for key in ["content", "prompt", "input", "message", "msg", "text", "query"] {
            let mut e = entity(SemanticRole::SystemPrompt, "");
            e.extracted_fields.clear();
            e.extracted_fields
                .insert(key.to_string(), serde_json::json!("Research AI safety trends"));
            assert_eq!(
                extract(&[e]).as_deref(),
                Some("Research AI safety trends"),
                "{key} must be recognised",
            );
        }
    }

    // ── Normalisation ────────────────────────────────────────────────────────

    #[test]
    fn whitespace_is_collapsed() {
        let out = extract(&[entity(
            SemanticRole::SystemPrompt,
            "  You   are\n\ta helpful\n  assistant  ",
        )]);
        assert_eq!(out.as_deref(), Some("You are a helpful assistant"));
    }

    #[test]
    fn text_that_is_too_short_is_rejected() {
        // "ok" would produce a vector that matches everything weakly.
        assert_eq!(extract(&[entity(SemanticRole::SystemPrompt, "ok")]), None);
        assert_eq!(extract(&[entity(SemanticRole::SystemPrompt, "   ")]), None);
    }

    #[test]
    fn a_short_system_prompt_falls_through_to_the_user_turn() {
        let out = extract(&[
            entity(SemanticRole::SystemPrompt, "hi"),
            entity(SemanticRole::UserTurn, "Summarise the quarterly report"),
        ]);
        assert_eq!(out.as_deref(), Some("Summarise the quarterly report"));
    }

    #[test]
    fn over_long_text_is_truncated_not_dropped() {
        let long = "a ".repeat(3_000);
        let out = extract(&[entity(SemanticRole::SystemPrompt, &long)]).expect("truncated, not dropped");
        assert_eq!(out.chars().count(), MAX_INTENT_CHARS);
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        // Multi-byte input must not panic or produce invalid UTF-8.
        let long = "日本語のテキスト ".repeat(1_000);
        let out = extract(&[entity(SemanticRole::SystemPrompt, &long)]).unwrap();
        assert_eq!(out.chars().count(), MAX_INTENT_CHARS);
    }

    // ── Empty cases ──────────────────────────────────────────────────────────

    #[test]
    fn no_entities_yields_no_intent() {
        assert_eq!(extract(&[]), None);
    }

    #[test]
    fn entities_with_no_usable_text_yield_no_intent() {
        let mut e = entity(SemanticRole::SystemPrompt, "");
        e.extracted_fields.clear();
        assert_eq!(extract(&[e]), None);
    }
}
