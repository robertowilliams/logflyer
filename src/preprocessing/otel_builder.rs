//! Stage 10 — assemble OTel-compatible span records from entity data.
//!
//! Produces [`OtelSpan`] documents that conform to the [OpenTelemetry Trace]
//! data model.  Spans are stored in the `otel_spans` MongoDB collection by
//! the Phase 6 output adapter and can be exported via a future OTLP endpoint.
//!
//! [OpenTelemetry Trace]: https://opentelemetry.io/docs/specs/otel/trace/api/
//!
//! # Span naming convention
//! `{semantic_role}:{entity_type}` — both segments use the same `snake_case`
//! representation as the serde serialisation of [`SemanticRole`] and
//! [`EntityType`].  For example:
//! * `system_prompt:prompt_event`
//! * `assistant_turn:completion_event`
//! * `tool_invocation:tool_call_event`
//!
//! # Required UpsideGate span attributes
//! | Attribute key | Source field | Condition |
//! |---------------|--------------|-----------|
//! | `ug.entity.type` | `entity_type` | always |
//! | `ug.semantic.role` | `semantic_role` | always |
//! | `ug.sample.hash` | `sample_hash` | always |
//! | `ug.target.id` | `target_id` | always |
//! | `ug.tool.name` | `tool_name` | ToolCallEvent / ToolResultEvent |
//! | `ug.model.id` | `model_id` | CompletionEvent / PromptEvent |
//! | `ug.mcp.server` | `mcp_server_id` | McpEvent |
//! | `ug.token.count` | `token_count` | if present |
//! | `ug.latency.ms` | `latency_ms` | if present |

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::models::{EntityRecord, EntityType};

// ─── OTel types ───────────────────────────────────────────────────────────────

/// OTel span kind — matches the OTel SpanKind enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SpanKind {
    /// Default for internal processing steps (AgentStep, ContextWindow).
    Internal,
    /// Outbound calls to external services (ToolCallEvent, RetrievalEvent, McpEvent).
    Client,
    /// Inbound service entry points — unused in current entity set.
    Server,
    /// Async message producers (PromptEvent).
    Producer,
    /// Async message consumers (CompletionEvent, ToolResultEvent).
    Consumer,
}

/// OTel span status code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StatusCode {
    Unset,
    Ok,
    Error,
}

/// OTel span status (code + optional message).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanStatus {
    pub code: StatusCode,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub message: String,
}

impl Default for SpanStatus {
    fn default() -> Self {
        Self {
            code: StatusCode::Unset,
            message: String::new(),
        }
    }
}

/// A typed span attribute value.
///
/// OTel attributes are typed; this enum covers the subset used by UpsideGate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttributeValue {
    String(String),
    Int(i64),
    Bool(bool),
}

/// An OTel-compatible span record derived from a single [`EntityRecord`].
///
/// Stored in the `otel_spans` MongoDB collection; exportable via OTLP once
/// the Phase 6 output adapter is implemented.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtelSpan {
    /// 32-hex-char OTel trace ID (shared across all spans in a sample).
    pub trace_id: String,
    /// 16-hex-char OTel span ID (unique to this entity).
    pub span_id: String,
    /// Span ID of the logical parent entity, if any.
    pub parent_span_id: Option<String>,
    /// Human-readable span name: `{semantic_role}:{entity_type}`.
    pub name: String,
    pub kind: SpanKind,
    /// Unix epoch nanoseconds.  `0` when no timestamp was extractable.
    pub start_time_unix_nano: u64,
    /// `start_time_unix_nano + latency_ms * 1_000_000`.  Equal to
    /// `start_time_unix_nano` when latency is unknown.
    pub end_time_unix_nano: u64,
    /// UpsideGate span attributes (see module docs for the full list).
    pub attributes: HashMap<String, AttributeValue>,
    pub status: SpanStatus,
    /// FK to the parent sample — enables efficient range queries.
    pub sample_hash: String,
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Map an [`EntityType`] to the appropriate OTel [`SpanKind`].
fn span_kind(entity_type: &EntityType) -> SpanKind {
    match entity_type {
        EntityType::PromptEvent => SpanKind::Producer,
        EntityType::CompletionEvent | EntityType::ToolResultEvent => SpanKind::Consumer,
        EntityType::ToolCallEvent | EntityType::RetrievalEvent | EntityType::McpEvent => {
            SpanKind::Client
        }
        EntityType::AgentStep | EntityType::ContextWindow | EntityType::Unknown => {
            SpanKind::Internal
        }
    }
}

/// Serialise an enum variant to its serde string representation.
///
/// Both [`EntityType`] and [`SemanticRole`] use `#[serde(rename_all = "snake_case")]`,
/// so this function returns the canonical snake_case label (e.g. `"prompt_event"`).
fn enum_str<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Build the `{semantic_role}:{entity_type}` span name.
fn span_name(entity: &EntityRecord) -> String {
    format!(
        "{}:{}",
        enum_str(&entity.semantic_role),
        enum_str(&entity.entity_type),
    )
}

/// Build the attribute map for a span.
fn build_attributes(entity: &EntityRecord) -> HashMap<String, AttributeValue> {
    let mut attrs: HashMap<String, AttributeValue> = HashMap::new();

    // Always-present attributes
    attrs.insert(
        "ug.entity.type".to_string(),
        AttributeValue::String(enum_str(&entity.entity_type)),
    );
    attrs.insert(
        "ug.semantic.role".to_string(),
        AttributeValue::String(enum_str(&entity.semantic_role)),
    );
    attrs.insert(
        "ug.sample.hash".to_string(),
        AttributeValue::String(entity.sample_hash.clone()),
    );
    attrs.insert(
        "ug.target.id".to_string(),
        AttributeValue::String(entity.target_id.clone()),
    );

    // Conditional attributes
    if let Some(ref tool) = entity.tool_name {
        attrs.insert(
            "ug.tool.name".to_string(),
            AttributeValue::String(tool.clone()),
        );
    }
    if let Some(ref model) = entity.model_id {
        attrs.insert(
            "ug.model.id".to_string(),
            AttributeValue::String(model.clone()),
        );
    }
    if let Some(ref mcp) = entity.mcp_server_id {
        attrs.insert(
            "ug.mcp.server".to_string(),
            AttributeValue::String(mcp.clone()),
        );
    }
    if let Some(tokens) = entity.token_count {
        attrs.insert(
            "ug.token.count".to_string(),
            AttributeValue::Int(tokens as i64),
        );
    }
    if let Some(latency) = entity.latency_ms {
        attrs.insert(
            "ug.latency.ms".to_string(),
            AttributeValue::Int(latency as i64),
        );
    }

    attrs
}

/// Derive start/end timestamps in Unix nanoseconds.
///
/// * `start` — from `entity.timestamp_utc` if available; otherwise `0`.
/// * `end`   — `start + latency_ms * 1_000_000`; falls back to `start`.
fn timestamps(entity: &EntityRecord) -> (u64, u64) {
    let start: u64 = entity
        .timestamp_utc
        .map(|dt| {
            // bson::DateTime::timestamp_millis() returns i64; saturate on negatives.
            let ms = dt.timestamp_millis();
            if ms >= 0 { ms as u64 * 1_000_000 } else { 0 }
        })
        .unwrap_or(0);

    let end: u64 = entity
        .latency_ms
        .map(|lat| start.saturating_add(lat * 1_000_000))
        .unwrap_or(start);

    (start, end)
}

// ─── Public entry point ───────────────────────────────────────────────────────

/// Build one [`OtelSpan`] per [`EntityRecord`].
///
/// Spans share `trace_id` (set once per sample by [`super::mod`]) and carry
/// `parent_span_id` when the entity extractor identified a parent relationship.
///
/// # Arguments
/// * `entities`    – typed, classified entities from Stages 6–7
/// * `sample_hash` – stable hash of the parent sample (redundant with
///   `entity.sample_hash` but kept for ergonomic use in aggregation queries)
pub fn build(entities: &[EntityRecord], sample_hash: &str) -> Vec<OtelSpan> {
    entities
        .iter()
        .map(|e| {
            let (start, end) = timestamps(e);
            OtelSpan {
                trace_id: e.trace_id.clone(),
                span_id: e.span_id.clone(),
                parent_span_id: e.parent_span_id.clone(),
                name: span_name(e),
                kind: span_kind(&e.entity_type),
                start_time_unix_nano: start,
                end_time_unix_nano: end,
                attributes: build_attributes(e),
                status: SpanStatus::default(),
                sample_hash: sample_hash.to_string(),
            }
        })
        .collect()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::models::{EntityRecord, EntityType, SemanticRole};

    // ── Entity builder ────────────────────────────────────────────────────────

    fn make_entity(entity_type: EntityType, semantic_role: SemanticRole) -> EntityRecord {
        make_entity_full(entity_type, semantic_role, None, None, None, None, None, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn make_entity_full(
        entity_type: EntityType,
        semantic_role: SemanticRole,
        tool_name: Option<&str>,
        model_id: Option<&str>,
        mcp_server_id: Option<&str>,
        token_count: Option<u32>,
        latency_ms: Option<u64>,
        parent_span_id: Option<&str>,
    ) -> EntityRecord {
        EntityRecord {
            entity_id: "eid-test".to_string(),
            entity_type,
            semantic_role,
            sample_hash: "testhash".to_string(),
            target_id: "target-001".to_string(),
            trace_id: "aabbccdd00112233aabbccdd00112233".to_string(),
            span_id: "abcd1234ef567890".to_string(),
            parent_span_id: parent_span_id.map(str::to_string),
            prov_entity_id: "ug:entity:eid-test".to_string(),
            prov_activity_id: "ug:activity:testhash:0".to_string(),
            line_index: 0,
            raw_text: String::new(),
            extracted_fields: HashMap::new(),
            model_id: model_id.map(str::to_string),
            tool_name: tool_name.map(str::to_string),
            mcp_server_id: mcp_server_id.map(str::to_string),
            token_count,
            latency_ms,
            timestamp_utc: None,
            content_embedding_id: None,
            behavioral_embedding_id: None,
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // One span per entity
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn build_returns_one_span_per_entity() {
        let entities = vec![
            make_entity(EntityType::PromptEvent, SemanticRole::SystemPrompt),
            make_entity(EntityType::CompletionEvent, SemanticRole::AssistantTurn),
            make_entity(EntityType::AgentStep, SemanticRole::AgentReasoning),
        ];
        let spans = build(&entities, "sh");
        assert_eq!(spans.len(), 3);
    }

    #[test]
    fn build_empty_entities_returns_empty() {
        let spans = build(&[], "sh");
        assert!(spans.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Span names
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn span_name_system_prompt() {
        let e = make_entity(EntityType::PromptEvent, SemanticRole::SystemPrompt);
        let spans = build(&[e], "sh");
        assert_eq!(spans[0].name, "system_prompt:prompt_event");
    }

    #[test]
    fn span_name_assistant_turn() {
        let e = make_entity(EntityType::CompletionEvent, SemanticRole::AssistantTurn);
        let spans = build(&[e], "sh");
        assert_eq!(spans[0].name, "assistant_turn:completion_event");
    }

    #[test]
    fn span_name_tool_invocation() {
        let e = make_entity(EntityType::ToolCallEvent, SemanticRole::ToolInvocation);
        let spans = build(&[e], "sh");
        assert_eq!(spans[0].name, "tool_invocation:tool_call_event");
    }

    #[test]
    fn span_name_agent_reasoning() {
        let e = make_entity(EntityType::AgentStep, SemanticRole::AgentReasoning);
        let spans = build(&[e], "sh");
        assert_eq!(spans[0].name, "agent_reasoning:agent_step");
    }

    #[test]
    fn span_name_mcp_request() {
        let e = make_entity(EntityType::McpEvent, SemanticRole::McpRequest);
        let spans = build(&[e], "sh");
        assert_eq!(spans[0].name, "mcp_request:mcp_event");
    }

    #[test]
    fn span_name_unknown_role() {
        let e = make_entity(EntityType::Unknown, SemanticRole::Unknown);
        let spans = build(&[e], "sh");
        assert_eq!(spans[0].name, "unknown:unknown");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // SpanKind mapping
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn span_kind_prompt_event_is_producer() {
        let e = make_entity(EntityType::PromptEvent, SemanticRole::Unknown);
        assert_eq!(build(&[e], "sh")[0].kind, SpanKind::Producer);
    }

    #[test]
    fn span_kind_completion_event_is_consumer() {
        let e = make_entity(EntityType::CompletionEvent, SemanticRole::Unknown);
        assert_eq!(build(&[e], "sh")[0].kind, SpanKind::Consumer);
    }

    #[test]
    fn span_kind_tool_call_event_is_client() {
        let e = make_entity(EntityType::ToolCallEvent, SemanticRole::Unknown);
        assert_eq!(build(&[e], "sh")[0].kind, SpanKind::Client);
    }

    #[test]
    fn span_kind_tool_result_event_is_consumer() {
        let e = make_entity(EntityType::ToolResultEvent, SemanticRole::Unknown);
        assert_eq!(build(&[e], "sh")[0].kind, SpanKind::Consumer);
    }

    #[test]
    fn span_kind_retrieval_event_is_client() {
        let e = make_entity(EntityType::RetrievalEvent, SemanticRole::Unknown);
        assert_eq!(build(&[e], "sh")[0].kind, SpanKind::Client);
    }

    #[test]
    fn span_kind_mcp_event_is_client() {
        let e = make_entity(EntityType::McpEvent, SemanticRole::Unknown);
        assert_eq!(build(&[e], "sh")[0].kind, SpanKind::Client);
    }

    #[test]
    fn span_kind_agent_step_is_internal() {
        let e = make_entity(EntityType::AgentStep, SemanticRole::Unknown);
        assert_eq!(build(&[e], "sh")[0].kind, SpanKind::Internal);
    }

    #[test]
    fn span_kind_context_window_is_internal() {
        let e = make_entity(EntityType::ContextWindow, SemanticRole::Unknown);
        assert_eq!(build(&[e], "sh")[0].kind, SpanKind::Internal);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Trace / span IDs and parent propagation
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn span_carries_trace_and_span_ids() {
        let e = make_entity(EntityType::PromptEvent, SemanticRole::Unknown);
        let span = &build(&[e], "sh")[0];
        assert_eq!(span.trace_id, "aabbccdd00112233aabbccdd00112233");
        assert_eq!(span.span_id, "abcd1234ef567890");
    }

    #[test]
    fn parent_span_id_propagated_when_set() {
        let e = make_entity_full(
            EntityType::ToolCallEvent,
            SemanticRole::ToolInvocation,
            None,
            None,
            None,
            None,
            None,
            Some("parentspan0001"),
        );
        let span = &build(&[e], "sh")[0];
        assert_eq!(span.parent_span_id, Some("parentspan0001".to_string()));
    }

    #[test]
    fn parent_span_id_none_when_not_set() {
        let e = make_entity(EntityType::PromptEvent, SemanticRole::SystemPrompt);
        let span = &build(&[e], "sh")[0];
        assert!(span.parent_span_id.is_none());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Attributes — always-present
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn attributes_always_contain_four_required_keys() {
        let e = make_entity(EntityType::AgentStep, SemanticRole::AgentReasoning);
        let span = &build(&[e], "sh")[0];
        assert!(span.attributes.contains_key("ug.entity.type"));
        assert!(span.attributes.contains_key("ug.semantic.role"));
        assert!(span.attributes.contains_key("ug.sample.hash"));
        assert!(span.attributes.contains_key("ug.target.id"));
    }

    #[test]
    fn attribute_entity_type_value() {
        let e = make_entity(EntityType::CompletionEvent, SemanticRole::AssistantTurn);
        let span = &build(&[e], "sh")[0];
        assert_eq!(
            span.attributes["ug.entity.type"],
            AttributeValue::String("completion_event".to_string())
        );
    }

    #[test]
    fn attribute_semantic_role_value() {
        let e = make_entity(EntityType::CompletionEvent, SemanticRole::AssistantTurn);
        let span = &build(&[e], "sh")[0];
        assert_eq!(
            span.attributes["ug.semantic.role"],
            AttributeValue::String("assistant_turn".to_string())
        );
    }

    #[test]
    fn attribute_sample_hash_value() {
        let e = make_entity(EntityType::PromptEvent, SemanticRole::Unknown);
        let span = &build(&[e], "myhash999")[0];
        assert_eq!(
            span.attributes["ug.sample.hash"],
            AttributeValue::String("testhash".to_string()) // comes from entity, not param
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Attributes — conditional
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn tool_name_attribute_set_when_present() {
        let e = make_entity_full(
            EntityType::ToolCallEvent,
            SemanticRole::ToolInvocation,
            Some("web_search"),
            None,
            None,
            None,
            None,
            None,
        );
        let span = &build(&[e], "sh")[0];
        assert_eq!(
            span.attributes["ug.tool.name"],
            AttributeValue::String("web_search".to_string())
        );
    }

    #[test]
    fn tool_name_attribute_absent_when_none() {
        let e = make_entity(EntityType::PromptEvent, SemanticRole::Unknown);
        let span = &build(&[e], "sh")[0];
        assert!(!span.attributes.contains_key("ug.tool.name"));
    }

    #[test]
    fn model_id_attribute_set_when_present() {
        let e = make_entity_full(
            EntityType::CompletionEvent,
            SemanticRole::AssistantTurn,
            None,
            Some("claude-3-opus"),
            None,
            None,
            None,
            None,
        );
        let span = &build(&[e], "sh")[0];
        assert_eq!(
            span.attributes["ug.model.id"],
            AttributeValue::String("claude-3-opus".to_string())
        );
    }

    #[test]
    fn mcp_server_attribute_set_when_present() {
        let e = make_entity_full(
            EntityType::McpEvent,
            SemanticRole::McpRequest,
            None,
            None,
            Some("filesystem-mcp"),
            None,
            None,
            None,
        );
        let span = &build(&[e], "sh")[0];
        assert_eq!(
            span.attributes["ug.mcp.server"],
            AttributeValue::String("filesystem-mcp".to_string())
        );
    }

    #[test]
    fn token_count_attribute_set_when_present() {
        let e = make_entity_full(
            EntityType::CompletionEvent,
            SemanticRole::AssistantTurn,
            None,
            None,
            None,
            Some(1842),
            None,
            None,
        );
        let span = &build(&[e], "sh")[0];
        assert_eq!(
            span.attributes["ug.token.count"],
            AttributeValue::Int(1842)
        );
    }

    #[test]
    fn latency_ms_attribute_set_when_present() {
        let e = make_entity_full(
            EntityType::CompletionEvent,
            SemanticRole::AssistantTurn,
            None,
            None,
            None,
            None,
            Some(1823),
            None,
        );
        let span = &build(&[e], "sh")[0];
        assert_eq!(
            span.attributes["ug.latency.ms"],
            AttributeValue::Int(1823)
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Timestamps
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn timestamps_zero_when_no_utc_timestamp() {
        let e = make_entity(EntityType::PromptEvent, SemanticRole::Unknown);
        let span = &build(&[e], "sh")[0];
        assert_eq!(span.start_time_unix_nano, 0);
        assert_eq!(span.end_time_unix_nano, 0);
    }

    #[test]
    fn end_time_is_start_plus_latency_nanos() {
        // Build an entity with latency but no timestamp → start=0, end=latency*1e6
        let e = make_entity_full(
            EntityType::CompletionEvent,
            SemanticRole::AssistantTurn,
            None,
            None,
            None,
            None,
            Some(500), // 500 ms
            None,
        );
        let span = &build(&[e], "sh")[0];
        assert_eq!(span.start_time_unix_nano, 0);
        assert_eq!(span.end_time_unix_nano, 500 * 1_000_000);
    }

    #[test]
    fn start_equals_end_when_no_latency() {
        let e = make_entity(EntityType::AgentStep, SemanticRole::AgentReasoning);
        let span = &build(&[e], "sh")[0];
        assert_eq!(span.start_time_unix_nano, span.end_time_unix_nano);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // sample_hash on the span
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn span_carries_sample_hash_from_param() {
        let e = make_entity(EntityType::PromptEvent, SemanticRole::Unknown);
        let span = &build(&[e], "paramhash42")[0];
        assert_eq!(span.sample_hash, "paramhash42");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Status default
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn span_status_defaults_to_unset() {
        let e = make_entity(EntityType::PromptEvent, SemanticRole::Unknown);
        let span = &build(&[e], "sh")[0];
        assert_eq!(span.status.code, StatusCode::Unset);
        assert!(span.status.message.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Integration: openai fixture shape (7 entities)
    // ─────────────────────────────────────────────────────────────────────────

    fn openai_entities() -> Vec<EntityRecord> {
        vec![
            make_entity(EntityType::PromptEvent, SemanticRole::SystemPrompt),
            make_entity(EntityType::PromptEvent, SemanticRole::UserTurn),
            make_entity_full(
                EntityType::ToolCallEvent,
                SemanticRole::ToolInvocation,
                Some("web_search"),
                None,
                None,
                None,
                None,
                None,
            ),
            make_entity(EntityType::RetrievalEvent, SemanticRole::RetrievalQuery),
            make_entity_full(
                EntityType::ToolResultEvent,
                SemanticRole::ToolResponse,
                Some("web_search"),
                None,
                None,
                None,
                Some(38),
                None,
            ),
            make_entity(EntityType::ContextWindow, SemanticRole::ContextAssembly),
            make_entity_full(
                EntityType::CompletionEvent,
                SemanticRole::AssistantTurn,
                None,
                Some("gpt-4o"),
                None,
                Some(2154),
                Some(1823),
                None,
            ),
        ]
    }

    #[test]
    fn openai_fixture_produces_seven_spans() {
        let spans = build(&openai_entities(), "oh");
        assert_eq!(spans.len(), 7);
    }

    #[test]
    fn openai_fixture_span_names_are_role_colon_type() {
        let spans = build(&openai_entities(), "oh");
        assert_eq!(spans[0].name, "system_prompt:prompt_event");
        assert_eq!(spans[1].name, "user_turn:prompt_event");
        assert_eq!(spans[2].name, "tool_invocation:tool_call_event");
        assert_eq!(spans[3].name, "retrieval_query:retrieval_event");
        assert_eq!(spans[4].name, "tool_response:tool_result_event");
        assert_eq!(spans[5].name, "context_assembly:context_window");
        assert_eq!(spans[6].name, "assistant_turn:completion_event");
    }

    #[test]
    fn openai_fixture_completion_has_model_and_tokens() {
        let spans = build(&openai_entities(), "oh");
        let completion = &spans[6];
        assert_eq!(
            completion.attributes["ug.model.id"],
            AttributeValue::String("gpt-4o".to_string())
        );
        assert_eq!(
            completion.attributes["ug.token.count"],
            AttributeValue::Int(2154)
        );
        // latency on completion span: end = 1823 * 1_000_000
        assert_eq!(completion.end_time_unix_nano, 1823 * 1_000_000);
    }

    #[test]
    fn openai_fixture_tool_result_has_latency() {
        let spans = build(&openai_entities(), "oh");
        let result = &spans[4]; // ToolResultEvent with latency_ms=38
        assert_eq!(result.end_time_unix_nano, 38 * 1_000_000);
    }
}
