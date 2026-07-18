//! Stage 6 — parse log lines into typed [`EntityRecord`] instances.
//!
//! The extractor iterates non-empty lines and runs three passes per sample:
//!
//! **Pass 1 — Type detection.**  Each line is scored against a prioritised set
//! of compiled regular expressions.  The highest-priority matching pattern
//! determines the [`EntityType`].  Lines that match no pattern are skipped.
//!
//! **Pass 2 — Field extraction.**  For JSON lines the extractor deserialises
//! the object and pulls out known fields (`model`, `tool_name`, `usage.*`,
//! `latency_ms`, etc.).  For Logfmt lines it splits on `key=value` pairs.
//! PlainText lines use regex captures.  All extracted key/value pairs are
//! stored in `extracted_fields`.
//!
//! **Pass 3 — Span parent inference.**  After all entities are built the
//! extractor links child spans to their logical parents based on entity-type
//! pair rules (e.g. `ToolResultEvent` → nearest `ToolCallEvent` with the same
//! `tool_name`).

use std::collections::HashMap;

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

use crate::models::{AgenticScan, EntityRecord, EntityType, LogFormat, LogSchema, LogType, SemanticRole};
use super::ids;
use super::mcp_parser;

// ── Detection patterns ────────────────────────────────────────────────────────

struct TypePattern {
    entity_type: EntityType,
    /// Higher value wins when multiple patterns match the same line.
    priority: u8,
    regex: Regex,
}

impl TypePattern {
    fn new(entity_type: EntityType, priority: u8, pattern: &str) -> Self {
        Self {
            entity_type,
            priority,
            regex: Regex::new(pattern).expect("entity type pattern must compile"),
        }
    }
}

/// Prioritised list of entity-type detection patterns.
///
/// Evaluation order: all patterns are checked; the one with the highest
/// `priority` value that matches wins.  Ties are broken by list order (first
/// match with equal priority wins).
static TYPE_PATTERNS: Lazy<Vec<TypePattern>> = Lazy::new(|| {
    vec![
        // ── McpEvent — highest priority because JSON-RPC "2.0" is unambiguous ─
        TypePattern::new(
            EntityType::McpEvent,
            10,
            r#"(?i)"jsonrpc"\s*:\s*"2\.0""#,
        ),
        // ── CompletionEvent ───────────────────────────────────────────────────
        TypePattern::new(
            EntityType::CompletionEvent,
            9,
            r#"(?i)(finish_reason|completion_tokens|"role"\s*:\s*"assistant"|AssistantMessage)"#,
        ),
        // ── ToolResultEvent ───────────────────────────────────────────────────
        TypePattern::new(
            EntityType::ToolResultEvent,
            8,
            r#"(?i)(tool_result|function_result|"type"\s*:\s*"tool_result"|Observation\s*:|Action\s+Output\s*:)"#,
        ),
        // ── ToolCallEvent ─────────────────────────────────────────────────────
        TypePattern::new(
            EntityType::ToolCallEvent,
            8,
            r#"(?i)(tool_call|function_call|"type"\s*:\s*"tool_use"|tool_use\b|Action\s*:|Action\s+Input\s*:)"#,
        ),
        // ── RetrievalEvent ────────────────────────────────────────────────────
        TypePattern::new(
            EntityType::RetrievalEvent,
            7,
            r"(?i)(similarity.?search|vector.?store|retrieved.?chunks?|RAG\b|cosine.?sim|nearest.?neighbor|retrieval.?augmented|embedding.?lookup|augment.*context|retrieved.?document)",
        ),
        // ── ContextWindow ─────────────────────────────────────────────────────
        TypePattern::new(
            EntityType::ContextWindow,
            6,
            r"(?i)(context[_.]window|context.?window.?exceeded|context.?assembly|assembled.?context|context.?overflow|token.?limit.?exceeded)",
        ),
        // ── PromptEvent ───────────────────────────────────────────────────────
        TypePattern::new(
            EntityType::PromptEvent,
            6,
            r#"(?i)("role"\s*:\s*"(user|system)"|HumanMessage|SystemMessage|system_message|system.?prompt|role\s*=\s*(user|system)\b)"#,
        ),
        // ── AgentStep ─────────────────────────────────────────────────────────
        TypePattern::new(
            EntityType::AgentStep,
            5,
            r"(?i)(Thought\s*:|AgentExecutor\b|AgentStep\b|crew\.kickoff|step\s*=\s*\d|PLAN\b|REFLECT\b|Final\s+Answer\s*:)",
        ),
        // ── ToolCallEvent — lower-priority plain-text signals ─────────────────
        TypePattern::new(
            EntityType::ToolCallEvent,
            4,
            r"(?i)(calling.?tool|invoke.?tool|tool.?invoked|tool_name\s*=\s*\S+)",
        ),
        // ── McpEvent — broad MCP method signals ───────────────────────────────
        TypePattern::new(
            EntityType::McpEvent,
            3,
            r#"(?i)"method"\s*:\s*"(initialize|tools/|resources/|prompts/|sampling/)"#,
        ),
    ]
});

// ── Field extraction regexes ──────────────────────────────────────────────────

static LOGFMT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(\w[\w.-]*)\s*=\s*(?:"([^"]*)"|(\S+))"#).unwrap()
});
static MODEL_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)"?model(?:_id)?"?\s*[=:]\s*"?([a-z0-9._/:+-]{2,64})"?"#).unwrap()
});
static TOOL_NAME_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)(?:tool[_.]?name|Action)\s*[=:]\s*"?([a-z0-9_.-]+)"?"#).unwrap()
});
static TOKEN_COUNT_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:total_tokens|completion_tokens|prompt_tokens)\s*[=:]\s*(\d+)").unwrap()
});
static LATENCY_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:duration_ms|latency_ms|elapsed_ms|duration|latency|elapsed)\s*[=:]\s*(\d+)")
        .unwrap()
});
static MCP_SERVER_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)"?(?:server_?id|mcp_?server|serverId)"?\s*[=:]\s*"?([a-z0-9._:-]+)"?"#)
        .unwrap()
});

// ── Public interface ──────────────────────────────────────────────────────────

/// Extract typed entity records from `content`.
///
/// Returns an empty `Vec` when `content` is blank or no lines match any
/// entity-type pattern.  All entities share the caller-supplied `trace_id`.
///
/// # Arguments
/// * `content`     – raw log sample text
/// * `format`      – detected log format from Stage 1
/// * `schema`      – extracted field schema from Stage 4 (structured logs only)
/// * `agentic`     – agentic scan result from Stage 3
/// * `sample_hash` – stable hash of the parent `SampleRecord`
/// * `target_id`   – identifier of the sampling target
/// * `trace_id`    – OTel trace id shared across all entities in this sample
pub fn extract(
    content: &str,
    format: &LogFormat,
    _schema: &Option<LogSchema>,
    _agentic: &AgenticScan,
    sample_hash: &str,
    target_id: &str,
    trace_id: &str,
) -> Vec<EntityRecord> {
    if content.trim().is_empty() {
        return vec![];
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut entities: Vec<EntityRecord> = Vec::new();

    let is_json = format.log_type == LogType::Json;
    extract_lines(&lines, sample_hash, target_id, trace_id, is_json, &mut entities);

    // Pass 3: infer parent-child span relationships
    infer_parent_spans(&mut entities);

    entities
}

// ── Pass 1 + 2: line iteration ────────────────────────────────────────────────

fn extract_lines(
    lines: &[&str],
    sample_hash: &str,
    target_id: &str,
    trace_id: &str,
    is_json: bool,
    entities: &mut Vec<EntityRecord>,
) {
    for (raw_index, &line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let entity_type = detect_entity_type(trimmed);
        if entity_type == EntityType::Unknown {
            continue;
        }

        let fields = if is_json {
            extract_json_fields(trimmed)
        } else {
            extract_logfmt_fields(trimmed)
        };

        let line_index = raw_index as u32;
        // Content-derived IDs so re-running the pipeline upserts cleanly
        // instead of inserting duplicates (see `preprocessing::ids`).
        let entity_id = ids::derive_entity_id(sample_hash, line_index, trimmed);
        let span_id   = ids::derive_span_id(sample_hash, line_index);

        let model_id = extract_model_id(trimmed, &fields);

        // For McpEvent lines use the dedicated JSON-RPC 2.0 parser (Phase 7)
        // for higher-fidelity field extraction, then fall back to the generic
        // extractors for any field the MCP parser did not populate.
        let (tool_name, mcp_server_id) = if entity_type == EntityType::McpEvent {
            let mcp = mcp_parser::parse(trimmed);
            let tn = mcp.as_ref().and_then(|m| m.tool_name.clone())
                .or_else(|| extract_tool_name(trimmed, &fields, &entity_type));
            let ms = mcp.as_ref().and_then(|m| m.server_id.clone())
                .or_else(|| extract_mcp_server_id(trimmed, &fields));
            (tn, ms)
        } else {
            (
                extract_tool_name(trimmed, &fields, &entity_type),
                extract_mcp_server_id(trimmed, &fields),
            )
        };

        let token_count = extract_token_count(trimmed, &fields);
        let latency_ms = extract_latency_ms(trimmed, &fields);

        entities.push(EntityRecord {
            prov_entity_id: format!("ug:entity:{}", entity_id),
            prov_activity_id: format!("ug:activity:{}:{}", sample_hash, line_index),
            entity_id,
            entity_type,
            semantic_role: SemanticRole::Unknown, // Stage 7 fills this in
            sample_hash: sample_hash.to_string(),
            target_id: target_id.to_string(),
            trace_id: trace_id.to_string(),
            span_id,
            parent_span_id: None, // Pass 3 fills this in
            line_index,
            raw_text: trimmed.to_string(),
            extracted_fields: fields,
            model_id,
            tool_name,
            mcp_server_id,
            token_count,
            latency_ms,
            timestamp_utc: None,
            content_embedding_id: None,
            behavioral_embedding_id: None,
        });
    }
}

// ── Pass 1: entity type detection ────────────────────────────────────────────

/// Return the highest-priority [`EntityType`] matched by any pattern, or
/// [`EntityType::Unknown`] if no pattern matches.
fn detect_entity_type(line: &str) -> EntityType {
    let mut best_type = EntityType::Unknown;
    let mut best_priority = 0u8;

    for pattern in TYPE_PATTERNS.iter() {
        if pattern.priority > best_priority && pattern.regex.is_match(line) {
            best_priority = pattern.priority;
            best_type = pattern.entity_type.clone();
        }
    }

    best_type
}

// ── Pass 2: field extraction ──────────────────────────────────────────────────

fn extract_json_fields(line: &str) -> HashMap<String, Value> {
    match serde_json::from_str::<Value>(line) {
        Ok(Value::Object(map)) => map.into_iter().collect(),
        _ => HashMap::new(),
    }
}

fn extract_logfmt_fields(line: &str) -> HashMap<String, Value> {
    let mut map = HashMap::new();
    for cap in LOGFMT_RE.captures_iter(line) {
        let key = cap[1].to_string();
        // Group 2 = quoted value, group 3 = bare value
        let val = cap
            .get(2)
            .or_else(|| cap.get(3))
            .map(|m| m.as_str())
            .unwrap_or("");
        map.insert(key, Value::String(val.to_string()));
    }
    map
}

fn extract_model_id(line: &str, fields: &HashMap<String, Value>) -> Option<String> {
    // Structured field lookup first
    for key in &["model", "model_id", "modelId", "model-id"] {
        if let Some(Value::String(v)) = fields.get(*key) {
            if !v.is_empty() && v != "null" {
                return Some(v.clone());
            }
        }
    }
    // Regex fallback on raw line
    MODEL_REGEX
        .captures(line)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

fn extract_tool_name(
    line: &str,
    fields: &HashMap<String, Value>,
    entity_type: &EntityType,
) -> Option<String> {
    if matches!(
        entity_type,
        EntityType::ToolCallEvent | EntityType::ToolResultEvent | EntityType::McpEvent
    ) {
        // Flat structured field
        for key in &["tool_name", "tool", "name", "function_name"] {
            if let Some(Value::String(v)) = fields.get(*key) {
                if !v.is_empty() && v != "null" {
                    return Some(v.clone());
                }
            }
        }
        // OpenAI tool_calls[0].function.name
        if let Some(Value::Array(calls)) = fields.get("tool_calls") {
            if let Some(first) = calls.first() {
                if let Value::Object(call) = first {
                    if let Some(Value::Object(func)) = call.get("function") {
                        if let Some(Value::String(name)) = func.get("name") {
                            return Some(name.clone());
                        }
                    }
                }
            }
        }
        // MCP tools/call: params.name
        if let Some(Value::Object(params)) = fields.get("params") {
            if let Some(Value::String(name)) = params.get("name") {
                return Some(name.clone());
            }
        }
    }
    // Plain-text: "Action: web_search" or "tool_name=search"
    TOOL_NAME_REGEX
        .captures(line)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

fn extract_token_count(line: &str, fields: &HashMap<String, Value>) -> Option<u32> {
    // Check nested usage object (OpenAI API format)
    if let Some(Value::Object(usage)) = fields.get("usage") {
        for key in &["total_tokens", "prompt_tokens", "completion_tokens"] {
            if let Some(v) = usage.get(*key) {
                if let Some(n) = v.as_u64() {
                    return Some(n as u32);
                }
            }
        }
    }
    // Flat integer fields
    for key in &["total_tokens", "prompt_tokens", "completion_tokens", "token_count", "tokens"] {
        if let Some(v) = fields.get(*key) {
            if let Some(n) = v.as_u64() {
                return Some(n as u32);
            }
            // Logfmt stores everything as String
            if let Value::String(s) = v {
                if let Ok(n) = s.parse::<u32>() {
                    return Some(n);
                }
            }
        }
    }
    // Regex fallback
    TOKEN_COUNT_REGEX
        .captures(line)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

fn extract_latency_ms(line: &str, fields: &HashMap<String, Value>) -> Option<u64> {
    for key in &["duration_ms", "latency_ms", "elapsed_ms", "latency", "duration", "elapsed"] {
        if let Some(v) = fields.get(*key) {
            if let Some(n) = v.as_u64() {
                return Some(n);
            }
            if let Value::String(s) = v {
                if let Ok(n) = s.parse::<u64>() {
                    return Some(n);
                }
            }
        }
    }
    LATENCY_REGEX
        .captures(line)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

fn extract_mcp_server_id(line: &str, fields: &HashMap<String, Value>) -> Option<String> {
    for key in &["server_id", "mcp_server", "serverId", "mcp_server_id"] {
        if let Some(Value::String(v)) = fields.get(*key) {
            if !v.is_empty() {
                return Some(v.clone());
            }
        }
    }
    // MCP initialize response: result.serverInfo.name
    if let Some(Value::Object(result)) = fields.get("result") {
        if let Some(Value::Object(info)) = result.get("serverInfo") {
            if let Some(Value::String(name)) = info.get("name") {
                return Some(name.clone());
            }
        }
    }
    // Flat serverInfo object
    if let Some(Value::Object(info)) = fields.get("serverInfo") {
        if let Some(Value::String(name)) = info.get("name") {
            return Some(name.clone());
        }
    }
    MCP_SERVER_REGEX
        .captures(line)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

// ── Pass 3: span parent inference ─────────────────────────────────────────────

/// Infer parent-child span relationships between entities.
///
/// Rules applied:
/// - `ToolResultEvent` → nearest `ToolCallEvent` with matching `tool_name`
///   (falls back to nearest `ToolCallEvent` if no name match).
/// - `CompletionEvent` → nearest preceding `PromptEvent`.
/// - `AgentStep` → nearest preceding `AgentStep` (chain).
/// - `McpEvent` with a `"result"` or `"error"` field → nearest preceding
///   `McpEvent` that is a request (no `"result"`/`"error"`).
fn infer_parent_spans(entities: &mut Vec<EntityRecord>) {
    // Build the parent map with a read-only pass to avoid borrow conflicts.
    let parents: Vec<Option<String>> = (0..entities.len())
        .map(|i| find_parent_span(entities, i))
        .collect();

    for (i, parent) in parents.into_iter().enumerate() {
        entities[i].parent_span_id = parent;
    }
}

fn find_parent_span(entities: &[EntityRecord], i: usize) -> Option<String> {
    match &entities[i].entity_type {
        EntityType::ToolResultEvent => {
            let tool_name = entities[i].tool_name.clone();
            // Prefer ToolCallEvent with the same tool_name
            if let Some(ref tn) = tool_name {
                for j in (0..i).rev() {
                    if entities[j].entity_type == EntityType::ToolCallEvent
                        && entities[j].tool_name.as_deref() == Some(tn.as_str())
                    {
                        return Some(entities[j].span_id.clone());
                    }
                }
            }
            // Fallback: nearest ToolCallEvent regardless of name
            for j in (0..i).rev() {
                if entities[j].entity_type == EntityType::ToolCallEvent {
                    return Some(entities[j].span_id.clone());
                }
            }
            None
        }

        EntityType::CompletionEvent => {
            for j in (0..i).rev() {
                if entities[j].entity_type == EntityType::PromptEvent {
                    return Some(entities[j].span_id.clone());
                }
            }
            None
        }

        EntityType::AgentStep => {
            for j in (0..i).rev() {
                if entities[j].entity_type == EntityType::AgentStep {
                    return Some(entities[j].span_id.clone());
                }
            }
            None
        }

        EntityType::McpEvent => {
            // Link MCP responses to their request
            let raw = &entities[i].raw_text;
            if raw.contains("\"result\"") || raw.contains("\"error\"") {
                for j in (0..i).rev() {
                    if entities[j].entity_type == EntityType::McpEvent
                        && !entities[j].raw_text.contains("\"result\"")
                        && !entities[j].raw_text.contains("\"error\"")
                    {
                        return Some(entities[j].span_id.clone());
                    }
                }
            }
            None
        }

        _ => None,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AgenticScan, LogFormat, LogType};

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("fixture not found: {}", path.display()))
    }

    fn json_format() -> LogFormat {
        LogFormat {
            log_type: LogType::Json,
            timestamp_field: None,
            level_field: None,
            message_field: None,
            timestamp_format: None,
            multiline: false,
        }
    }

    fn plain_format() -> LogFormat {
        LogFormat {
            log_type: LogType::PlainText,
            timestamp_field: None,
            level_field: None,
            message_field: None,
            timestamp_format: None,
            multiline: false,
        }
    }

    fn logfmt_format() -> LogFormat {
        LogFormat {
            log_type: LogType::Logfmt,
            timestamp_field: None,
            level_field: None,
            message_field: None,
            timestamp_format: None,
            multiline: false,
        }
    }

    fn empty_agentic() -> AgenticScan {
        AgenticScan {
            signal_score: 1.0,
            worth_classifying: true,
            detected_frameworks: vec![],
            matched_patterns: vec![],
            agentic_line_count: 1,
        }
    }

    // ── Pass 1: detect_entity_type ────────────────────────────────────────────

    #[test]
    fn test_detect_prompt_event_system_role() {
        assert_eq!(
            detect_entity_type(r#"{"role":"system","content":"You are helpful."}"#),
            EntityType::PromptEvent
        );
    }

    #[test]
    fn test_detect_prompt_event_user_role() {
        assert_eq!(
            detect_entity_type(r#"{"role":"user","content":"Hello!"}"#),
            EntityType::PromptEvent
        );
    }

    #[test]
    fn test_detect_prompt_event_human_message() {
        assert_eq!(
            detect_entity_type("HumanMessage: What is the weather today?"),
            EntityType::PromptEvent
        );
    }

    #[test]
    fn test_detect_completion_event_finish_reason() {
        assert_eq!(
            detect_entity_type(r#"{"finish_reason":"stop","usage":{}}"#),
            EntityType::CompletionEvent
        );
    }

    #[test]
    fn test_detect_completion_event_assistant_role() {
        assert_eq!(
            detect_entity_type(r#"{"role":"assistant","content":"Here is my answer."}"#),
            EntityType::CompletionEvent
        );
    }

    #[test]
    fn test_detect_completion_event_completion_tokens() {
        assert_eq!(
            detect_entity_type(r#"{"completion_tokens":128,"prompt_tokens":512}"#),
            EntityType::CompletionEvent
        );
    }

    #[test]
    fn test_detect_tool_call_event_keyword() {
        assert_eq!(
            detect_entity_type(r#"{"msg":"tool_call","tool_name":"search"}"#),
            EntityType::ToolCallEvent
        );
    }

    #[test]
    fn test_detect_tool_call_event_function_call() {
        assert_eq!(
            detect_entity_type(r#"{"function_call":{"name":"web_search","arguments":"{}"}}"#),
            EntityType::ToolCallEvent
        );
    }

    #[test]
    fn test_detect_tool_call_event_action_colon() {
        assert_eq!(
            detect_entity_type("Action: web_search"),
            EntityType::ToolCallEvent
        );
    }

    #[test]
    fn test_detect_tool_result_event_keyword() {
        assert_eq!(
            detect_entity_type(r#"{"msg":"tool_result","tool_name":"search","output":"result"}"#),
            EntityType::ToolResultEvent
        );
    }

    #[test]
    fn test_detect_tool_result_event_observation() {
        assert_eq!(
            detect_entity_type("Observation: The weather is 72°F and sunny."),
            EntityType::ToolResultEvent
        );
    }

    #[test]
    fn test_detect_retrieval_event_similarity_search() {
        assert_eq!(
            detect_entity_type(r#"{"msg":"vector_store similarity_search","k":5}"#),
            EntityType::RetrievalEvent
        );
    }

    #[test]
    fn test_detect_retrieval_event_rag() {
        assert_eq!(
            detect_entity_type("Retrieval augmented context built from 3 chunks"),
            EntityType::RetrievalEvent
        );
    }

    #[test]
    fn test_detect_agent_step_thought() {
        assert_eq!(
            detect_entity_type("Thought: I need to search for the latest news."),
            EntityType::AgentStep
        );
    }

    #[test]
    fn test_detect_agent_step_executor() {
        assert_eq!(
            detect_entity_type(r#"{"msg":"AgentExecutor running","tool_count":3}"#),
            EntityType::AgentStep
        );
    }

    #[test]
    fn test_detect_agent_step_final_answer() {
        assert_eq!(
            detect_entity_type("Final Answer: The answer is 42."),
            EntityType::AgentStep
        );
    }

    #[test]
    fn test_detect_mcp_event_jsonrpc() {
        assert_eq!(
            detect_entity_type(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#),
            EntityType::McpEvent
        );
    }

    #[test]
    fn test_detect_mcp_event_result() {
        assert_eq!(
            detect_entity_type(r#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok"}}"#),
            EntityType::McpEvent
        );
    }

    #[test]
    fn test_detect_context_window() {
        assert_eq!(
            detect_entity_type(
                r#"{"event":"context_window","assembled":true,"prompt_tokens":1842}"#
            ),
            EntityType::ContextWindow
        );
    }

    #[test]
    fn test_detect_unknown_plain_log() {
        assert_eq!(
            detect_entity_type(r#"{"level":"info","msg":"server started","port":8080}"#),
            EntityType::Unknown
        );
    }

    #[test]
    fn test_completion_beats_prompt_for_assistant_role() {
        // "role":"assistant" → CompletionEvent (P9) beats PromptEvent (P6)
        let line = r#"{"role":"assistant","content":"Here is my reply."}"#;
        assert_eq!(detect_entity_type(line), EntityType::CompletionEvent);
    }

    #[test]
    fn test_mcp_beats_tool_call() {
        // JSON-RPC 2.0 signature (P10) beats tool_call signal (P8)
        let line = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_file"}}"#;
        assert_eq!(detect_entity_type(line), EntityType::McpEvent);
    }

    // ── Pass 2: field extraction ──────────────────────────────────────────────

    #[test]
    fn test_extract_model_id_from_json() {
        let line = r#"{"model":"gpt-4o","finish_reason":"stop"}"#;
        let fields = extract_json_fields(line);
        assert_eq!(extract_model_id(line, &fields), Some("gpt-4o".to_string()));
    }

    #[test]
    fn test_extract_tool_name_from_flat_field() {
        let line = r#"{"tool_name":"web_search","msg":"tool_call"}"#;
        let fields = extract_json_fields(line);
        assert_eq!(
            extract_tool_name(line, &fields, &EntityType::ToolCallEvent),
            Some("web_search".to_string())
        );
    }

    #[test]
    fn test_extract_tool_name_from_openai_tool_calls() {
        let line = r#"{"tool_calls":[{"id":"call_1","type":"function","function":{"name":"calculator","arguments":"{}"}}]}"#;
        let fields = extract_json_fields(line);
        assert_eq!(
            extract_tool_name(line, &fields, &EntityType::ToolCallEvent),
            Some("calculator".to_string())
        );
    }

    #[test]
    fn test_extract_tool_name_from_action_colon() {
        let line = "Action: web_search";
        assert_eq!(
            extract_tool_name(line, &HashMap::new(), &EntityType::ToolCallEvent),
            Some("web_search".to_string())
        );
    }

    #[test]
    fn test_extract_token_count_nested_usage() {
        let line = r#"{"usage":{"prompt_tokens":512,"completion_tokens":128,"total_tokens":640}}"#;
        let fields = extract_json_fields(line);
        assert_eq!(extract_token_count(line, &fields), Some(640));
    }

    #[test]
    fn test_extract_token_count_flat_field() {
        let line = r#"{"completion_tokens":128,"msg":"done"}"#;
        let fields = extract_json_fields(line);
        assert_eq!(extract_token_count(line, &fields), Some(128));
    }

    #[test]
    fn test_extract_latency_from_json() {
        let line = r#"{"latency_ms":1823,"model":"gpt-4o"}"#;
        let fields = extract_json_fields(line);
        assert_eq!(extract_latency_ms(line, &fields), Some(1823));
    }

    #[test]
    fn test_extract_latency_from_logfmt() {
        let line = "level=info msg=done duration_ms=4921";
        let fields = extract_logfmt_fields(line);
        assert_eq!(extract_latency_ms(line, &fields), Some(4921));
    }

    #[test]
    fn test_extract_mcp_server_id_from_result() {
        let line = r#"{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"filesystem-mcp","version":"1.0"}}}"#;
        let fields = extract_json_fields(line);
        assert_eq!(
            extract_mcp_server_id(line, &fields),
            Some("filesystem-mcp".to_string())
        );
    }

    // ── Pass 3: parent span inference ─────────────────────────────────────────

    #[test]
    fn test_tool_result_links_to_tool_call() {
        let content = "Action: web_search\nObservation: The result is here.";
        let entities =
            extract(content, &plain_format(), &None, &empty_agentic(), "h", "t", "trace1");
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].entity_type, EntityType::ToolCallEvent);
        assert_eq!(entities[1].entity_type, EntityType::ToolResultEvent);
        assert_eq!(entities[1].parent_span_id, Some(entities[0].span_id.clone()));
    }

    #[test]
    fn test_completion_links_to_prompt() {
        let content = concat!(
            r#"{"role":"user","content":"Hello"}"#,
            "\n",
            r#"{"role":"assistant","content":"Hi there","finish_reason":"stop"}"#
        );
        let entities =
            extract(content, &json_format(), &None, &empty_agentic(), "h", "t", "trace1");
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].entity_type, EntityType::PromptEvent);
        assert_eq!(entities[1].entity_type, EntityType::CompletionEvent);
        assert_eq!(entities[1].parent_span_id, Some(entities[0].span_id.clone()));
    }

    #[test]
    fn test_agent_steps_form_chain() {
        let content =
            "Thought: step one\nAction: search\nObservation: found\nThought: step two\nFinal Answer: done";
        let entities =
            extract(content, &plain_format(), &None, &empty_agentic(), "h", "t", "trace1");

        let steps: Vec<&EntityRecord> =
            entities.iter().filter(|e| e.entity_type == EntityType::AgentStep).collect();
        assert!(steps.len() >= 2, "expected at least 2 AgentStep entities, got {}", steps.len());
        assert_eq!(steps[1].parent_span_id, Some(steps[0].span_id.clone()));
    }

    #[test]
    fn test_mcp_response_links_to_request() {
        let content = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"test-mcp"}}}"#
        );
        let entities =
            extract(content, &json_format(), &None, &empty_agentic(), "h", "t", "trace1");
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[1].parent_span_id, Some(entities[0].span_id.clone()));
    }

    // ── Integration: entity counts per fixture ────────────────────────────────

    #[test]
    fn test_openai_fixture_entity_types() {
        let content = fixture("openai_chat_completions.log");
        let entities =
            extract(&content, &json_format(), &None, &empty_agentic(), "h", "t", "trace1");

        assert!(!entities.is_empty(), "should extract entities from openai fixture");
        let types: Vec<EntityType> = entities.iter().map(|e| e.entity_type.clone()).collect();
        assert!(types.contains(&EntityType::PromptEvent), "expected PromptEvent: {types:?}");
        assert!(types.contains(&EntityType::CompletionEvent), "expected CompletionEvent: {types:?}");
        assert!(types.contains(&EntityType::ToolCallEvent), "expected ToolCallEvent: {types:?}");
        assert!(types.contains(&EntityType::ToolResultEvent), "expected ToolResultEvent: {types:?}");
        assert!(types.contains(&EntityType::RetrievalEvent), "expected RetrievalEvent: {types:?}");
        assert!(types.contains(&EntityType::ContextWindow), "expected ContextWindow: {types:?}");
    }

    #[test]
    fn test_openai_fixture_metadata_fields() {
        let content = fixture("openai_chat_completions.log");
        let entities =
            extract(&content, &json_format(), &None, &empty_agentic(), "h", "t", "trace1");

        for e in &entities {
            assert_eq!(e.trace_id, "trace1");
            assert_eq!(e.sample_hash, "h");
            assert!(!e.span_id.is_empty());
        }
        assert!(
            entities.iter().any(|e| e.model_id.as_deref() == Some("gpt-4o")),
            "expected gpt-4o model_id"
        );
        assert!(
            entities.iter().any(|e| e.token_count.is_some()),
            "expected at least one token_count"
        );
        assert!(
            entities.iter().any(|e| e.latency_ms.is_some()),
            "expected at least one latency_ms"
        );
    }

    #[test]
    fn test_react_fixture_entity_types() {
        let content = fixture("react_agent.log");
        let entities =
            extract(&content, &plain_format(), &None, &empty_agentic(), "h", "t", "trace1");

        assert!(!entities.is_empty(), "should extract entities from react_agent fixture");
        let types: Vec<EntityType> = entities.iter().map(|e| e.entity_type.clone()).collect();
        assert!(types.contains(&EntityType::AgentStep), "expected AgentStep: {types:?}");
        assert!(types.contains(&EntityType::ToolCallEvent), "expected ToolCallEvent: {types:?}");
        assert!(types.contains(&EntityType::ToolResultEvent), "expected ToolResultEvent: {types:?}");
    }

    #[test]
    fn test_react_fixture_tool_names() {
        let content = fixture("react_agent.log");
        let entities =
            extract(&content, &plain_format(), &None, &empty_agentic(), "h", "t", "trace1");

        let tool_calls: Vec<&EntityRecord> =
            entities.iter().filter(|e| e.entity_type == EntityType::ToolCallEvent).collect();
        assert!(
            tool_calls.len() >= 2,
            "expected ≥2 ToolCallEvents (web_search, calculator), got {}",
            tool_calls.len()
        );
        let names: Vec<&str> =
            tool_calls.iter().filter_map(|e| e.tool_name.as_deref()).collect();
        assert!(names.contains(&"web_search"), "expected web_search in tool names: {names:?}");
        assert!(names.contains(&"calculator"), "expected calculator in tool names: {names:?}");
    }

    #[test]
    fn test_mcp_fixture_all_mcp_events() {
        let content = fixture("mcp_jsonrpc.log");
        let entities =
            extract(&content, &json_format(), &None, &empty_agentic(), "h", "t", "trace1");

        assert!(!entities.is_empty(), "should extract entities from mcp fixture");
        assert!(
            entities.iter().all(|e| e.entity_type == EntityType::McpEvent),
            "all entities in MCP fixture should be McpEvent, got: {:?}",
            entities.iter().map(|e| &e.entity_type).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_mcp_fixture_responses_have_parents() {
        let content = fixture("mcp_jsonrpc.log");
        let entities =
            extract(&content, &json_format(), &None, &empty_agentic(), "h", "t", "trace1");

        let responses: Vec<&EntityRecord> =
            entities.iter().filter(|e| e.parent_span_id.is_some()).collect();
        assert!(!responses.is_empty(), "MCP responses should have parent span IDs");
    }

    #[test]
    fn test_mcp_fixture_server_id() {
        let content = fixture("mcp_jsonrpc.log");
        let entities =
            extract(&content, &json_format(), &None, &empty_agentic(), "h", "t", "trace1");

        assert!(
            entities.iter().any(|e| e.mcp_server_id.as_deref() == Some("filesystem-mcp")),
            "expected filesystem-mcp server_id"
        );
    }

    #[test]
    fn test_langchain_fixture_entities() {
        let content = fixture("langchain_json.log");
        let entities =
            extract(&content, &json_format(), &None, &empty_agentic(), "h", "t", "trace1");

        assert!(!entities.is_empty());
        let types: Vec<EntityType> = entities.iter().map(|e| e.entity_type.clone()).collect();
        assert!(types.contains(&EntityType::AgentStep), "{types:?}");
        assert!(types.contains(&EntityType::ToolCallEvent), "{types:?}");
        assert!(types.contains(&EntityType::ToolResultEvent), "{types:?}");
        assert!(types.contains(&EntityType::CompletionEvent), "{types:?}");
    }

    #[test]
    fn test_crewai_fixture_entities() {
        let content = fixture("crewai_logfmt.log");
        let entities =
            extract(&content, &logfmt_format(), &None, &empty_agentic(), "h", "t", "trace1");

        assert!(!entities.is_empty());
        let types: Vec<EntityType> = entities.iter().map(|e| e.entity_type.clone()).collect();
        assert!(types.contains(&EntityType::AgentStep), "{types:?}");
        assert!(types.contains(&EntityType::ToolCallEvent), "{types:?}");
        assert!(types.contains(&EntityType::ToolResultEvent), "{types:?}");
        assert!(types.contains(&EntityType::CompletionEvent), "{types:?}");
    }

    #[test]
    fn test_empty_content_returns_empty() {
        let entities =
            extract("", &json_format(), &None, &empty_agentic(), "h", "t", "trace1");
        assert!(entities.is_empty());
    }

    #[test]
    fn test_prov_uris_are_correct() {
        let content = "Thought: Let me think.";
        let entities =
            extract(content, &plain_format(), &None, &empty_agentic(), "myhash", "t", "trace1");
        assert_eq!(entities.len(), 1);
        let e = &entities[0];
        assert!(
            e.prov_entity_id.starts_with("ug:entity:"),
            "bad prov_entity_id: {}",
            e.prov_entity_id
        );
        assert!(
            e.prov_activity_id.starts_with("ug:activity:myhash:"),
            "bad prov_activity_id: {}",
            e.prov_activity_id
        );
    }

    #[test]
    fn test_span_ids_are_unique() {
        let content = "Thought: one\nThought: two\nThought: three";
        let entities =
            extract(content, &plain_format(), &None, &empty_agentic(), "h", "t", "trace1");
        assert_eq!(entities.len(), 3);
        let ids: std::collections::HashSet<&str> =
            entities.iter().map(|e| e.span_id.as_str()).collect();
        assert_eq!(ids.len(), 3, "span_ids should be unique");
    }

    #[test]
    fn test_nginx_returns_no_entities() {
        let content = fixture("nginx_access.log");
        let entities =
            extract(&content, &plain_format(), &None, &empty_agentic(), "h", "t", "trace1");
        assert!(
            entities.is_empty(),
            "nginx log should produce no entities, got {} with types {:?}",
            entities.len(),
            entities.iter().map(|e| &e.entity_type).collect::<Vec<_>>()
        );
    }

    // ── mcp_session.log — multi-turn session (Phase 7) ────────────────────────

    #[test]
    fn test_mcp_session_fixture_all_mcp_events() {
        // The fixture exercises every JSON-RPC message shape: requests,
        // responses (success and error), and notifications (no `id`).
        let content = fixture("mcp_session.log");
        let entities =
            extract(&content, &json_format(), &None, &empty_agentic(), "h", "t", "trace1");

        assert!(!entities.is_empty(), "should extract entities from mcp_session fixture");
        assert!(
            entities.iter().all(|e| e.entity_type == EntityType::McpEvent),
            "all entities in mcp_session fixture should be McpEvent, got: {:?}",
            entities.iter().map(|e| &e.entity_type).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_mcp_session_fixture_extracts_server_id() {
        let content = fixture("mcp_session.log");
        let entities =
            extract(&content, &json_format(), &None, &empty_agentic(), "h", "t", "trace1");

        assert!(
            entities.iter().any(|e| e.mcp_server_id.as_deref() == Some("acme-mcp-server")),
            "expected acme-mcp-server server_id, got: {:?}",
            entities.iter().filter_map(|e| e.mcp_server_id.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_mcp_session_fixture_tool_calls_extract_names() {
        let content = fixture("mcp_session.log");
        let entities =
            extract(&content, &json_format(), &None, &empty_agentic(), "h", "t", "trace1");

        let tool_names: std::collections::HashSet<&str> = entities
            .iter()
            .filter_map(|e| e.tool_name.as_deref())
            .collect();

        for expected in &["web_search", "calculator", "read_file"] {
            assert!(
                tool_names.contains(expected),
                "expected tool name `{expected}` in extracted set: {tool_names:?}"
            );
        }
    }

    #[test]
    fn test_mcp_session_fixture_responses_link_to_requests() {
        let content = fixture("mcp_session.log");
        let entities =
            extract(&content, &json_format(), &None, &empty_agentic(), "h", "t", "trace1");

        // Every response or error line should have its `parent_span_id`
        // pointing at the matching request.  This exercises the
        // request-response inference branch of `find_parent_span`.
        let responses_with_parent = entities
            .iter()
            .filter(|e| {
                let raw = &e.raw_text;
                (raw.contains("\"result\"") || raw.contains("\"error\"")) && e.parent_span_id.is_some()
            })
            .count();
        assert!(
            responses_with_parent >= 5,
            "expected ≥5 response/error entities to have parent span IDs, got {responses_with_parent}; \
             entity_count={}",
            entities.len(),
        );
    }

    #[test]
    fn test_mcp_session_fixture_handles_notifications() {
        // Notifications have a `method` but no `id` field.  They must still
        // be detected as McpEvent and not crash the parser when the
        // request-response inference runs.
        let content = fixture("mcp_session.log");
        let entities =
            extract(&content, &json_format(), &None, &empty_agentic(), "h", "t", "trace1");

        // The fixture contains three notifications: initialized, progress,
        // and message.  All three must be present as McpEvent entities.
        let notification_lines: Vec<&str> = content
            .lines()
            .filter(|l| l.contains("notifications/"))
            .collect();
        assert_eq!(
            notification_lines.len(),
            3,
            "fixture sanity check: expected exactly 3 notification lines"
        );

        let notification_entities = entities
            .iter()
            .filter(|e| e.raw_text.contains("notifications/"))
            .count();
        assert_eq!(
            notification_entities,
            3,
            "all 3 notification lines should produce McpEvent entities"
        );
    }
}
