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
//! | `ug.finish.reason` | `finish_reason` | CompletionEvent, if present |

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::stats;
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
    // Recorded for every completion, including reasons that do not affect
    // status — notably `length`, which is how a truncated generation stays
    // queryable without being counted as an error. See
    // [`ABNORMAL_FINISH_REASONS`].
    if let Some(reason) = finish_reason(&entity.extracted_fields) {
        attrs.insert(
            "ug.finish.reason".to_string(),
            AttributeValue::String(reason),
        );
    }

    attrs
}

/// Derive the OTel status for a span from the entity's extracted fields.
///
/// `extracted_fields` holds the log line's top-level keys verbatim — unflattened
/// and untyped — so error signals arrive in several shapes depending on what
/// produced the line:
///
/// | Shape                              | Source                            |
/// |------------------------------------|-----------------------------------|
/// | `error: { code, message }`          | JSON-RPC 2.0 error envelope       |
/// | `error: "some message"` / `true`    | generic structured logging        |
/// | `result: { isError: true }`         | MCP tool-level failure convention |
/// | `level`/`severity: "error"`         | conventional log severity         |
/// | `finish_reason: "content_filter"`   | LLM refusal                       |
/// | `finish_reason: "stop"` / `"length"`| LLM completion outcome            |
///
/// Values arrive **untyped**: `extract_logfmt_fields` stores every value as a
/// `String`, so `error=false` reaches here as `"false"` rather than
/// `Bool(false)`. See [`FALSEY`] — without it the commonest spelling of "nothing
/// went wrong" would be read as a failure, exactly inverted.
///
/// `finish_reason` is also read from `choices[0]` as well as the top level,
/// because `extracted_fields` holds only top-level keys and the OpenAI wire
/// format nests it.
///
/// **Policy choice worth knowing about:** only `"content_filter"` counts as an
/// error — a refused generation is a failed operation. `"length"` does **not**:
/// hitting the token ceiling is routine for streaming and summarisation
/// workloads, and OTel reserves `Error` for operations that failed, so counting
/// truncation would inflate any error rate derived from span status. It is
/// recorded as the `ug.finish.reason` attribute instead, so it stays queryable
/// without being counted. That decision lives entirely in
/// [`ABNORMAL_FINISH_REASONS`], so reversing it is a one-line change.
///
/// `Ok` is set affirmatively when a line carries positive evidence of success,
/// rather than leaving everything non-erroring as `Unset` — it lets the
/// SpansView waterfall distinguish "succeeded" from "we cannot tell".
fn status(entity: &EntityRecord) -> SpanStatus {
    let fields = &entity.extracted_fields;

    // 1. Explicit error field, under any of its common spellings.
    for key in ERROR_KEYS {
        if let Some(value) = fields.get(*key) {
            if let Some(message) = error_message(value) {
                return SpanStatus { code: StatusCode::Error, message };
            }
        }
    }

    // 2. MCP tool-level failure: `result.isError == true`.
    if fields
        .get("result")
        .and_then(|r| r.as_object())
        .and_then(|r| r.get("isError"))
        .is_some_and(is_truthy)
    {
        return SpanStatus {
            code: StatusCode::Error,
            message: "tool reported isError".to_string(),
        };
    }

    // 3. Structured log severity.
    for key in LEVEL_KEYS {
        if let Some(level) = fields.get(*key).and_then(|v| v.as_str()) {
            if is_error_level(level) {
                return SpanStatus {
                    code: StatusCode::Error,
                    message: format!("{key}={level}"),
                };
            }
        }
    }

    // 4. Unstructured severity, for plain-text and multiline samples where the
    //    extractors produce no key/value pairs at all — without this, an
    //    `ERROR InvokeModel failed: ThrottlingException` line would be Unset.
    //    Reuses the scanner behind `SampleStats.level_distribution` so severity
    //    classification is not reimplemented here.
    if fields.is_empty() {
        if let Some(level) = stats::extract_level_plain(&entity.raw_text) {
            if is_error_level(&level) {
                return SpanStatus {
                    code: StatusCode::Error,
                    message: format!("level={level}"),
                };
            }
        }
    }

    // 5. LLM completion outcome.
    if let Some(reason) = finish_reason(fields) {
        let lowered = reason.to_ascii_lowercase();
        if ABNORMAL_FINISH_REASONS.contains(&lowered.as_str()) {
            return SpanStatus {
                code: StatusCode::Error,
                message: format!("finish_reason={reason}"),
            };
        }
        if NORMAL_FINISH_REASONS.contains(&lowered.as_str()) {
            return SpanStatus {
                code: StatusCode::Ok,
                message: String::new(),
            };
        }
    }

    SpanStatus::default()
}

/// Field names that carry an error indication.
///
/// `err` is the dominant Go/logfmt spelling and `exception` the common Python
/// one, so matching only `error` would miss most non-JSON-RPC sources.
const ERROR_KEYS: &[&str] = &["error", "err", "exception", "error_message", "errorMessage"];

/// Field names that carry a conventional log severity.
const LEVEL_KEYS: &[&str] = &["level", "severity", "log_level", "lvl", "loglevel"];

/// Values that look like "no error" once stringified.
///
/// This matters because [`extract_logfmt_fields`] types **every** value as a
/// string: a logfmt line reading `error=false` arrives as `"false"`, not
/// `Bool(false)`, so without this list the most common way of saying "there was
/// no error" would be read as an error — exactly inverted.
///
/// [`extract_logfmt_fields`]: super::entity_extractor
const FALSEY: &[&str] = &["", "null", "nil", "<nil>", "none", "false", "no", "0", "-", "n/a"];

/// Whether a value indicates an error, and if so the message to record.
///
/// Returns `None` when the value means "no error".
fn error_message(value: &serde_json::Value) -> Option<String> {
    use serde_json::Value;
    match value {
        // `error: null` is the JSON-RPC *success* spelling.
        Value::Null => None,
        Value::Bool(false) => None,
        Value::Bool(true) => Some("error".to_string()),
        // A zero error code conventionally means success.
        Value::Number(n) if n.as_f64() == Some(0.0) => None,
        Value::Number(n) => Some(format!("error code {n}")),
        Value::String(s) if FALSEY.contains(&s.trim().to_ascii_lowercase().as_str()) => None,
        Value::String(s) => Some(s.clone()),
        // JSON-RPC envelope: { "code": -32601, "message": "Method not found" }
        Value::Object(obj) => {
            let message = obj
                .get("message")
                .and_then(|m| m.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| "error".to_string());
            // Codes are usually numeric but may be symbolic (`"E_TIMEOUT"`).
            let code = obj.get("code").and_then(|c| match c {
                Value::Number(n) => Some(n.to_string()),
                Value::String(s) if !s.is_empty() => Some(s.clone()),
                _ => None,
            });
            Some(match code {
                Some(code) => format!("{message} (code {code})"),
                None => message,
            })
        }
        // An empty array is the "no errors" spelling used by some aggregators.
        Value::Array(items) if items.is_empty() => None,
        Value::Array(_) => Some("error".to_string()),
    }
}

/// Truthiness across both JSON booleans and logfmt's stringified ones.
fn is_truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::String(s) => !FALSEY.contains(&s.trim().to_ascii_lowercase().as_str()),
        serde_json::Value::Number(n) => n.as_f64() != Some(0.0),
        _ => false,
    }
}

/// Whether a severity string denotes failure.
///
/// Canonicalises through [`stats::extract_level_plain`] rather than holding its
/// own alias table, so `err`, `crit`, `emerg` and friends resolve the same way
/// they do in `SampleStats.level_distribution`.
fn is_error_level(level: &str) -> bool {
    let canonical = stats::extract_level_plain(level).unwrap_or_else(|| level.to_ascii_lowercase());
    matches!(
        canonical.as_str(),
        "error" | "critical" | "fatal" | "alert" | "emergency" | "panic"
    )
}

/// Find `finish_reason`, whether at the top level or nested under `choices`.
///
/// `extracted_fields` holds only the line's **top-level** keys, but the OpenAI
/// wire format puts the reason at `choices[0].finish_reason`. Without this the
/// completion-outcome branch would never fire on the most common shape there is.
fn finish_reason(fields: &HashMap<String, serde_json::Value>) -> Option<String> {
    if let Some(reason) = fields.get("finish_reason").and_then(|v| v.as_str()) {
        return Some(reason.to_string());
    }
    // `choices` is an array of per-candidate objects; the first is the one the
    // caller acted on.
    fields
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(|r| r.as_str())
        .map(str::to_string)
}

/// `finish_reason` values meaning the completion did not finish as intended.
///
/// Only `content_filter` — a refused generation is a failed operation. `length`
/// is deliberately **not** here: hitting the token ceiling is routine for
/// streaming and summarisation workloads, and OTel reserves `Error` for
/// operations that failed, so marking truncation as an error would inflate any
/// error rate derived from span status. The reason is still recorded as the
/// `ug.finish.reason` attribute, so truncation remains queryable without being
/// counted as a failure.
const ABNORMAL_FINISH_REASONS: &[&str] = &["content_filter"];

/// `finish_reason` values meaning the completion finished cleanly.
const NORMAL_FINISH_REASONS: &[&str] = &["stop", "tool_calls", "function_call", "end_turn"];

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
                status: status(e),
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
    // Status derivation
    // ─────────────────────────────────────────────────────────────────────────

    /// Build a one-span slice from an entity carrying `fields`.
    fn status_for(fields: serde_json::Value) -> SpanStatus {
        let mut e = make_entity(EntityType::CompletionEvent, SemanticRole::Unknown);
        e.extracted_fields = fields
            .as_object()
            .expect("test fields must be a JSON object")
            .clone()
            .into_iter()
            .collect();
        build(&[e], "sh")[0].status.clone()
    }

    #[test]
    fn span_status_is_unset_without_any_signal() {
        let e = make_entity(EntityType::PromptEvent, SemanticRole::Unknown);
        let span = &build(&[e], "sh")[0];
        assert_eq!(span.status.code, StatusCode::Unset);
        assert!(span.status.message.is_empty());
    }

    // ── JSON-RPC error envelope ──────────────────────────────────────────────

    #[test]
    fn jsonrpc_error_object_sets_error_with_code_and_message() {
        let s = status_for(serde_json::json!({
            "error": { "code": -32601, "message": "Method not found" }
        }));
        assert_eq!(s.code, StatusCode::Error);
        assert_eq!(s.message, "Method not found (code -32601)");
    }

    #[test]
    fn jsonrpc_error_object_without_code_still_errors() {
        let s = status_for(serde_json::json!({ "error": { "message": "boom" } }));
        assert_eq!(s.code, StatusCode::Error);
        assert_eq!(s.message, "boom");
    }

    #[test]
    fn null_error_field_is_the_jsonrpc_success_spelling() {
        // `{"error": null}` accompanies a successful JSON-RPC response and must
        // not be read as a failure.
        let s = status_for(serde_json::json!({ "error": null }));
        assert_eq!(s.code, StatusCode::Unset);
    }

    #[test]
    fn false_error_field_is_not_an_error() {
        let s = status_for(serde_json::json!({ "error": false }));
        assert_eq!(s.code, StatusCode::Unset);
    }

    #[test]
    fn empty_error_string_is_not_an_error() {
        assert_eq!(status_for(serde_json::json!({ "error": "" })).code, StatusCode::Unset);
        assert_eq!(status_for(serde_json::json!({ "error": "null" })).code, StatusCode::Unset);
    }

    // ── logfmt: every value arrives as a string ──────────────────────────────
    // `extract_logfmt_fields` types everything as Value::String, so the Bool
    // arms are unreachable for any non-JSON source. These cases are the ones
    // that actually occur in the wild.

    #[test]
    fn logfmt_error_false_is_not_an_error() {
        // The critical case: `error=false` is the commonest way of saying "no
        // error", and reading it as a failure would invert the meaning of the
        // line. It arrives as the STRING "false", never as Bool(false).
        let s = status_for(serde_json::json!({ "error": "false" }));
        assert_eq!(s.code, StatusCode::Unset);
    }

    #[test]
    fn logfmt_falsey_error_spellings_are_not_errors() {
        for spelling in ["false", "FALSE", "nil", "<nil>", "none", "None", "no", "0", "-", "n/a", " "] {
            assert_eq!(
                status_for(serde_json::json!({ "error": spelling })).code,
                StatusCode::Unset,
                "error={spelling:?} must not be treated as a failure",
            );
        }
    }

    #[test]
    fn logfmt_error_true_is_an_error() {
        assert_eq!(
            status_for(serde_json::json!({ "error": "true" })).code,
            StatusCode::Error,
        );
    }

    #[test]
    fn logfmt_is_error_string_is_respected() {
        // `result.isError` may also arrive stringified.
        assert_eq!(
            status_for(serde_json::json!({ "result": { "isError": "true" } })).code,
            StatusCode::Error,
        );
        assert_eq!(
            status_for(serde_json::json!({ "result": { "isError": "false" } })).code,
            StatusCode::Unset,
        );
    }

    // ── Alternate error key spellings ────────────────────────────────────────

    #[test]
    fn alternate_error_keys_are_recognised() {
        for key in ["error", "err", "exception", "error_message", "errorMessage"] {
            let s = status_for(serde_json::json!({ key: "it broke" }));
            assert_eq!(s.code, StatusCode::Error, "{key} must be recognised");
            assert_eq!(s.message, "it broke");
        }
    }

    // ── Numeric and array error values ──────────────────────────────────────

    #[test]
    fn zero_error_code_is_success() {
        assert_eq!(status_for(serde_json::json!({ "error": 0 })).code, StatusCode::Unset);
    }

    #[test]
    fn nonzero_error_code_is_an_error() {
        let s = status_for(serde_json::json!({ "error": 500 }));
        assert_eq!(s.code, StatusCode::Error);
        assert_eq!(s.message, "error code 500");
    }

    #[test]
    fn empty_error_array_is_not_an_error() {
        assert_eq!(status_for(serde_json::json!({ "error": [] })).code, StatusCode::Unset);
        assert_eq!(
            status_for(serde_json::json!({ "error": ["boom"] })).code,
            StatusCode::Error,
        );
    }

    #[test]
    fn symbolic_error_code_is_kept_in_the_message() {
        let s = status_for(serde_json::json!({
            "error": { "code": "E_TIMEOUT", "message": "upstream timed out" }
        }));
        assert_eq!(s.code, StatusCode::Error);
        assert_eq!(s.message, "upstream timed out (code E_TIMEOUT)");
    }

    #[test]
    fn error_string_becomes_the_status_message() {
        let s = status_for(serde_json::json!({ "error": "connection refused" }));
        assert_eq!(s.code, StatusCode::Error);
        assert_eq!(s.message, "connection refused");
    }

    // ── MCP tool-level failure ───────────────────────────────────────────────

    #[test]
    fn mcp_result_is_error_true_sets_error() {
        let s = status_for(serde_json::json!({
            "result": { "isError": true, "content": [] }
        }));
        assert_eq!(s.code, StatusCode::Error);
        assert_eq!(s.message, "tool reported isError");
    }

    #[test]
    fn mcp_result_is_error_false_is_not_an_error() {
        let s = status_for(serde_json::json!({
            "result": { "isError": false, "content": [] }
        }));
        assert_eq!(s.code, StatusCode::Unset);
    }

    #[test]
    fn mcp_result_without_is_error_is_not_an_error() {
        let s = status_for(serde_json::json!({ "result": { "content": [] } }));
        assert_eq!(s.code, StatusCode::Unset);
    }

    // ── Conventional log severity ────────────────────────────────────────────

    #[test]
    fn error_level_sets_error() {
        for key in ["level", "severity", "log_level", "lvl", "loglevel"] {
            let s = status_for(serde_json::json!({ key: "error" }));
            assert_eq!(s.code, StatusCode::Error, "{key} must be recognised");
            assert_eq!(s.message, format!("{key}=error"));
        }
    }

    #[test]
    fn error_level_matching_is_case_insensitive() {
        assert_eq!(status_for(serde_json::json!({ "level": "ERROR" })).code, StatusCode::Error);
        assert_eq!(status_for(serde_json::json!({ "level": "Fatal" })).code, StatusCode::Error);
    }

    #[test]
    fn benign_levels_do_not_set_error() {
        for level in ["info", "debug", "warn", "trace"] {
            assert_eq!(
                status_for(serde_json::json!({ "level": level })).code,
                StatusCode::Unset,
                "{level} must not be an error",
            );
        }
    }

    // ── LLM completion outcome ───────────────────────────────────────────────

    #[test]
    fn normal_finish_reason_sets_ok() {
        for reason in ["stop", "tool_calls", "function_call", "end_turn"] {
            let s = status_for(serde_json::json!({ "finish_reason": reason }));
            assert_eq!(s.code, StatusCode::Ok, "{reason} is a clean finish");
            assert!(s.message.is_empty());
        }
    }

    #[test]
    fn content_filter_finish_reason_sets_error() {
        let s = status_for(serde_json::json!({ "finish_reason": "content_filter" }));
        assert_eq!(s.code, StatusCode::Error, "a refused generation is a failure");
        assert_eq!(s.message, "finish_reason=content_filter");
    }

    #[test]
    fn length_finish_reason_is_not_an_error() {
        // Hitting the token ceiling is routine, and OTel reserves Error for
        // operations that failed — counting truncation would inflate any error
        // rate derived from span status.
        let s = status_for(serde_json::json!({ "finish_reason": "length" }));
        assert_eq!(s.code, StatusCode::Unset);
    }

    #[test]
    fn finish_reason_is_recorded_as_an_attribute_even_when_status_ignores_it() {
        // `length` must remain queryable despite not affecting status.
        let mut e = make_entity(EntityType::CompletionEvent, SemanticRole::Unknown);
        e.extracted_fields = serde_json::json!({ "finish_reason": "length" })
            .as_object()
            .unwrap()
            .clone()
            .into_iter()
            .collect();
        let span = &build(&[e], "sh")[0];
        assert_eq!(
            span.attributes.get("ug.finish.reason"),
            Some(&AttributeValue::String("length".to_string())),
        );
    }

    #[test]
    fn unrecognised_finish_reason_stays_unset() {
        let s = status_for(serde_json::json!({ "finish_reason": "something_new" }));
        assert_eq!(s.code, StatusCode::Unset);
    }

    // ── Nested finish_reason (the OpenAI wire shape) ─────────────────────────

    #[test]
    fn finish_reason_is_found_under_choices() {
        // `extracted_fields` holds only top-level keys, but OpenAI puts the
        // reason at choices[0].finish_reason — the commonest shape there is.
        let s = status_for(serde_json::json!({
            "choices": [ { "index": 0, "finish_reason": "stop" } ]
        }));
        assert_eq!(s.code, StatusCode::Ok);
    }

    #[test]
    fn nested_content_filter_still_errors() {
        let s = status_for(serde_json::json!({
            "choices": [ { "finish_reason": "content_filter" } ]
        }));
        assert_eq!(s.code, StatusCode::Error);
    }

    #[test]
    fn top_level_finish_reason_wins_over_nested() {
        let s = status_for(serde_json::json!({
            "finish_reason": "content_filter",
            "choices": [ { "finish_reason": "stop" } ],
        }));
        assert_eq!(s.code, StatusCode::Error);
    }

    #[test]
    fn empty_choices_array_is_harmless() {
        assert_eq!(
            status_for(serde_json::json!({ "choices": [] })).code,
            StatusCode::Unset,
        );
    }

    // ── Unstructured severity ────────────────────────────────────────────────

    /// Status for an entity with no structured fields, only `raw_text`.
    fn status_for_raw(raw: &str) -> SpanStatus {
        let mut e = make_entity(EntityType::Unknown, SemanticRole::Unknown);
        e.raw_text = raw.to_string();
        build(&[e], "sh")[0].status.clone()
    }

    #[test]
    fn plaintext_error_line_sets_error() {
        // Multiline / plain-text samples yield no key-value pairs at all, so
        // without a raw_text fallback every such span would be Unset.
        let s = status_for_raw("2026-04-26 10:00:02 ERROR InvokeModel failed: ThrottlingException");
        assert_eq!(s.code, StatusCode::Error);
    }

    #[test]
    fn plaintext_bracketed_fatal_sets_error() {
        assert_eq!(
            status_for_raw("[FATAL] could not open socket").code,
            StatusCode::Error,
        );
    }

    #[test]
    fn plaintext_info_line_stays_unset() {
        assert_eq!(
            status_for_raw("2026-04-26 10:00:01 INFO InvokeModel ok").code,
            StatusCode::Unset,
        );
    }

    #[test]
    fn structured_fields_suppress_the_raw_text_scan() {
        // With structured fields present, the word "error" appearing in a
        // message must not by itself fail the span — the level field is
        // authoritative.
        let mut e = make_entity(EntityType::CompletionEvent, SemanticRole::Unknown);
        e.raw_text = r#"{"level":"info","msg":"retrying after error"}"#.to_string();
        e.extracted_fields = serde_json::json!({ "level": "info", "msg": "retrying after error" })
            .as_object()
            .unwrap()
            .clone()
            .into_iter()
            .collect();
        assert_eq!(build(&[e], "sh")[0].status.code, StatusCode::Unset);
    }

    // ── Severity aliases shared with stats ──────────────────────────────────

    #[test]
    fn severity_aliases_resolve_like_the_stats_scanner() {
        for level in ["err", "crit", "emerg", "alert", "emergency", "critical", "fatal"] {
            assert_eq!(
                status_for(serde_json::json!({ "level": level })).code,
                StatusCode::Error,
                "{level} must count as a failure",
            );
        }
    }

    #[test]
    fn notice_and_warn_aliases_are_not_errors() {
        for level in ["notice", "wrn", "warning", "inf", "dbg", "trc"] {
            assert_eq!(
                status_for(serde_json::json!({ "level": level })).code,
                StatusCode::Unset,
                "{level} must not count as a failure",
            );
        }
    }

    // ── Precedence ───────────────────────────────────────────────────────────

    #[test]
    fn explicit_error_outranks_a_clean_finish_reason() {
        // A line can carry both; the error is the more specific signal.
        let s = status_for(serde_json::json!({
            "finish_reason": "stop",
            "error": { "message": "downstream failed" },
        }));
        assert_eq!(s.code, StatusCode::Error);
        assert_eq!(s.message, "downstream failed");
    }

    #[test]
    fn error_level_outranks_a_clean_finish_reason() {
        let s = status_for(serde_json::json!({
            "finish_reason": "stop",
            "level": "error",
        }));
        assert_eq!(s.code, StatusCode::Error);
    }

    // ── Serialisation ────────────────────────────────────────────────────────

    #[test]
    fn status_serialises_in_otel_screaming_snake_case() {
        let s = status_for(serde_json::json!({ "error": "x" }));
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["code"], "ERROR");
    }

    #[test]
    fn empty_status_message_is_omitted_from_the_document() {
        let s = status_for(serde_json::json!({ "finish_reason": "stop" }));
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["code"], "OK");
        assert!(
            json.get("message").is_none(),
            "an empty message must be skipped, not stored as \"\"",
        );
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
