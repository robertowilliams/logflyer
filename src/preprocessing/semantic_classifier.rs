//! Stage 7 — assign a [`SemanticRole`] to each [`EntityRecord`] in place.
//!
//! Most [`EntityType`] → [`SemanticRole`] mappings are 1:1.  The exceptions
//! are the types that carry sub-roles distinguishable from context:
//!
//! | Entity type       | Context signal               | Role assigned        |
//! |-------------------|------------------------------|----------------------|
//! | `PromptEvent`     | `role=system` / SystemMessage | `SystemPrompt`       |
//! | `PromptEvent`     | `role=user` / HumanMessage   | `UserTurn`           |
//! | `CompletionEvent` | (any)                        | `AssistantTurn`      |
//! | `ToolCallEvent`   | (any)                        | `ToolInvocation`     |
//! | `ToolResultEvent` | (any)                        | `ToolResponse`       |
//! | `RetrievalEvent`  | query / search fields        | `RetrievalQuery`     |
//! | `RetrievalEvent`  | result / chunk / document    | `RetrievalResult`    |
//! | `AgentStep`       | "Thought" in raw text        | `AgentReasoning`     |
//! | `AgentStep`       | "Action" in raw text         | `AgentAction`        |
//! | `AgentStep`       | "Observation" in raw text    | `AgentObservation`   |
//! | `McpEvent`        | `"method"` field present     | `McpRequest`         |
//! | `McpEvent`        | `"result"` / `"error"` field | `McpResponse`        |
//! | `ContextWindow`   | (any)                        | `ContextAssembly`    |
//!
//! When signals are contradictory or absent the role stays `Unknown`.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

use crate::models::{EntityRecord, EntityType, SemanticRole};

// ── Regexes used in role disambiguation ──────────────────────────────────────

static SYSTEM_ROLE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)("role"\s*:\s*"system"|role\s*=\s*system\b|SystemMessage|system_message|system.?prompt)"#).unwrap()
});
static USER_ROLE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)("role"\s*:\s*"user"|role\s*=\s*user\b|HumanMessage)"#).unwrap()
});
static THOUGHT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)Thought\s*:").unwrap()
});
static ACTION_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\bAction\s*:").unwrap()
});
static OBSERVATION_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)Observation\s*:").unwrap()
});
static QUERY_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)(query|search|lookup|embedding.?lookup|similarity.?search)"#).unwrap()
});
static RESULT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(result|retrieved|chunk|document|retrieved.?context|rag.?result)").unwrap()
});

// ── Public interface ──────────────────────────────────────────────────────────

/// Assign a [`SemanticRole`] to every entity produced by Stage 6.
///
/// Operates in-place.  Entities that already have a non-`Unknown` role are
/// left unchanged (idempotent).
pub fn classify(entities: &mut Vec<EntityRecord>) {
    for entity in entities.iter_mut() {
        if entity.semantic_role != SemanticRole::Unknown {
            continue; // already assigned by a previous pass
        }
        entity.semantic_role = assign_role(entity);
    }
}

// ── Per-entity role assignment ────────────────────────────────────────────────

fn assign_role(entity: &EntityRecord) -> SemanticRole {
    match entity.entity_type {
        EntityType::PromptEvent => classify_prompt(entity),
        EntityType::CompletionEvent => SemanticRole::AssistantTurn,
        EntityType::ToolCallEvent => SemanticRole::ToolInvocation,
        EntityType::ToolResultEvent => SemanticRole::ToolResponse,
        EntityType::RetrievalEvent => classify_retrieval(entity),
        EntityType::AgentStep => classify_agent_step(entity),
        EntityType::McpEvent => classify_mcp(entity),
        EntityType::ContextWindow => SemanticRole::ContextAssembly,
        EntityType::Unknown => SemanticRole::Unknown,
    }
}

/// Distinguish `SystemPrompt` from `UserTurn` for a [`PromptEvent`].
///
/// Priority:
/// 1. `extracted_fields["role"]` — most reliable for structured logs.
/// 2. Regex scan of `raw_text` — covers Logfmt, PlainText, and JSON logs
///    where the role is embedded in a message field.
/// 3. Presence of "system_prompt" or "SystemMessage" keywords.
/// 4. If both system and user signals are present → `Unknown` (ambiguous).
/// 5. Default (no signals at all) → `UserTurn` (the most common prompt role).
fn classify_prompt(entity: &EntityRecord) -> SemanticRole {
    let role_field = entity
        .extracted_fields
        .get("role")
        .and_then(|v| v.as_str())
        .map(str::to_lowercase);

    let is_system_field = role_field.as_deref() == Some("system");
    let is_user_field = role_field.as_deref() == Some("user");

    let raw = &entity.raw_text;
    let is_system_text = SYSTEM_ROLE_RE.is_match(raw);
    let is_user_text = USER_ROLE_RE.is_match(raw);

    let is_system = is_system_field || (is_system_text && !is_user_field);
    let is_user = is_user_field || (is_user_text && !is_system_field);

    match (is_system, is_user) {
        (true, false) => SemanticRole::SystemPrompt,
        (false, true) => SemanticRole::UserTurn,
        (false, false) => SemanticRole::UserTurn, // safe default
        (true, true) => SemanticRole::Unknown,    // contradictory signals
    }
}

/// Distinguish `RetrievalQuery` from `RetrievalResult` for a [`RetrievalEvent`].
///
/// Priority:
/// 1. Structured fields: presence of `query` / `k` (top-k) → query;
///    `chunk_count` / `results` / `retrieved` → result.
/// 2. Regex scan of `raw_text`.
/// 3. Default → `RetrievalQuery` (lookups outnumber result lines in practice).
fn classify_retrieval(entity: &EntityRecord) -> SemanticRole {
    // Check structured fields for unambiguous signals
    let has_query_field = entity.extracted_fields.contains_key("query")
        || entity.extracted_fields.contains_key("k")
        || entity.extracted_fields.contains_key("similarity_search");
    let has_result_field = entity.extracted_fields.contains_key("chunk_count")
        || entity.extracted_fields.contains_key("results")
        || entity.extracted_fields.contains_key("retrieved");

    if has_result_field && !has_query_field {
        return SemanticRole::RetrievalResult;
    }
    if has_query_field && !has_result_field {
        return SemanticRole::RetrievalQuery;
    }

    // Fallback: regex on raw text
    let raw = &entity.raw_text;
    let query_signal = QUERY_RE.is_match(raw);
    let result_signal = RESULT_RE.is_match(raw);

    match (query_signal, result_signal) {
        (true, false) => SemanticRole::RetrievalQuery,
        (false, true) => SemanticRole::RetrievalResult,
        _ => SemanticRole::RetrievalQuery, // default
    }
}

/// Distinguish `AgentReasoning`, `AgentAction`, and `AgentObservation`
/// for an [`AgentStep`].
///
/// Priority:
/// 1. `Thought:` → `AgentReasoning`
/// 2. `Action:` → `AgentAction`
/// 3. `Observation:` → `AgentObservation`
/// 4. `Final Answer:` → `AgentReasoning` (conclusion of the reasoning chain)
/// 5. Framework signals (`AgentExecutor`, `crew.kickoff`) → `AgentReasoning`
/// 6. Default → `AgentReasoning`
fn classify_agent_step(entity: &EntityRecord) -> SemanticRole {
    let raw = &entity.raw_text;
    if THOUGHT_RE.is_match(raw) {
        SemanticRole::AgentReasoning
    } else if ACTION_RE.is_match(raw) {
        SemanticRole::AgentAction
    } else if OBSERVATION_RE.is_match(raw) {
        SemanticRole::AgentObservation
    } else {
        // Final Answer, AgentExecutor, crew.kickoff, etc.
        SemanticRole::AgentReasoning
    }
}

/// Distinguish `McpRequest` from `McpResponse` for a [`McpEvent`].
///
/// A JSON-RPC message is a request when it carries a `"method"` field,
/// and a response when it carries `"result"` or `"error"`.
fn classify_mcp(entity: &EntityRecord) -> SemanticRole {
    let has_method = entity.extracted_fields.contains_key("method");
    let raw = &entity.raw_text;
    let has_result = raw.contains("\"result\"") || raw.contains("\"error\"");

    match (has_method, has_result) {
        (true, false) => SemanticRole::McpRequest,
        (false, true) => SemanticRole::McpResponse,
        (true, true) => SemanticRole::Unknown, // malformed — shouldn't occur in valid JSON-RPC
        (false, false) => SemanticRole::McpRequest, // notification or unknown structure
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AgenticScan, EntityType, LogFormat, LogType, SemanticRole};
    use crate::preprocessing::entity_extractor;
    use std::collections::HashMap;

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("fixture not found: {}", path.display()))
    }

    fn make_entity(entity_type: EntityType, raw_text: &str) -> EntityRecord {
        EntityRecord {
            entity_id: "test".to_string(),
            entity_type,
            semantic_role: SemanticRole::Unknown,
            sample_hash: "h".to_string(),
            target_id: "t".to_string(),
            trace_id: "trace".to_string(),
            span_id: "span".to_string(),
            parent_span_id: None,
            prov_entity_id: "ug:entity:test".to_string(),
            prov_activity_id: "ug:activity:h:0".to_string(),
            line_index: 0,
            raw_text: raw_text.to_string(),
            extracted_fields: HashMap::new(),
            model_id: None,
            tool_name: None,
            mcp_server_id: None,
            token_count: None,
            latency_ms: None,
            timestamp_utc: None,
            content_embedding_id: None,
            behavioral_embedding_id: None,
        }
    }

    fn make_entity_with_fields(
        entity_type: EntityType,
        raw_text: &str,
        fields: HashMap<String, Value>,
    ) -> EntityRecord {
        EntityRecord {
            extracted_fields: fields,
            ..make_entity(entity_type, raw_text)
        }
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

    // ── PromptEvent disambiguation ────────────────────────────────────────────

    #[test]
    fn test_prompt_system_role_field() {
        let mut fields = HashMap::new();
        fields.insert("role".to_string(), Value::String("system".to_string()));
        let mut e = make_entity_with_fields(
            EntityType::PromptEvent,
            r#"{"role":"system","content":"You are helpful."}"#,
            fields,
        );
        classify(&mut vec![e.clone()]);
        // call assign_role directly since classify takes a Vec
        assert_eq!(assign_role(&e), SemanticRole::SystemPrompt);
        let _ = e; // suppress unused warning
    }

    #[test]
    fn test_prompt_user_role_field() {
        let mut fields = HashMap::new();
        fields.insert("role".to_string(), Value::String("user".to_string()));
        let e = make_entity_with_fields(
            EntityType::PromptEvent,
            r#"{"role":"user","content":"Hello!"}"#,
            fields,
        );
        assert_eq!(assign_role(&e), SemanticRole::UserTurn);
    }

    #[test]
    fn test_prompt_system_role_from_raw_text() {
        let e = make_entity(
            EntityType::PromptEvent,
            r#"SystemMessage: You are a helpful assistant."#,
        );
        assert_eq!(assign_role(&e), SemanticRole::SystemPrompt);
    }

    #[test]
    fn test_prompt_user_role_from_raw_text() {
        let e = make_entity(
            EntityType::PromptEvent,
            "HumanMessage: What is the weather today?",
        );
        assert_eq!(assign_role(&e), SemanticRole::UserTurn);
    }

    #[test]
    fn test_prompt_system_keyword_in_text() {
        let e = make_entity(
            EntityType::PromptEvent,
            r#"{"role":"system","content":"Be concise."}"#,
        );
        assert_eq!(assign_role(&e), SemanticRole::SystemPrompt);
    }

    #[test]
    fn test_prompt_no_role_defaults_to_user_turn() {
        let e = make_entity(
            EntityType::PromptEvent,
            r#"{"content":"What is the capital of France?"}"#,
        );
        assert_eq!(assign_role(&e), SemanticRole::UserTurn);
    }

    // ── 1:1 mappings ─────────────────────────────────────────────────────────

    #[test]
    fn test_completion_is_assistant_turn() {
        let e = make_entity(
            EntityType::CompletionEvent,
            r#"{"role":"assistant","finish_reason":"stop"}"#,
        );
        assert_eq!(assign_role(&e), SemanticRole::AssistantTurn);
    }

    #[test]
    fn test_tool_call_is_tool_invocation() {
        let e = make_entity(
            EntityType::ToolCallEvent,
            r#"{"msg":"tool_call","tool_name":"search"}"#,
        );
        assert_eq!(assign_role(&e), SemanticRole::ToolInvocation);
    }

    #[test]
    fn test_tool_result_is_tool_response() {
        let e = make_entity(
            EntityType::ToolResultEvent,
            r#"{"msg":"tool_result","output":"result text"}"#,
        );
        assert_eq!(assign_role(&e), SemanticRole::ToolResponse);
    }

    #[test]
    fn test_context_window_is_context_assembly() {
        let e = make_entity(
            EntityType::ContextWindow,
            r#"{"event":"context_window","assembled":true}"#,
        );
        assert_eq!(assign_role(&e), SemanticRole::ContextAssembly);
    }

    #[test]
    fn test_unknown_entity_stays_unknown() {
        let e = make_entity(EntityType::Unknown, "some random log line");
        assert_eq!(assign_role(&e), SemanticRole::Unknown);
    }

    // ── RetrievalEvent disambiguation ─────────────────────────────────────────

    #[test]
    fn test_retrieval_query_from_query_field() {
        let mut fields = HashMap::new();
        fields.insert("query".to_string(), Value::String("AI safety".to_string()));
        fields.insert("k".to_string(), Value::Number(5.into()));
        let e = make_entity_with_fields(
            EntityType::RetrievalEvent,
            r#"{"query":"AI safety","k":5,"similarity_search":true}"#,
            fields,
        );
        assert_eq!(assign_role(&e), SemanticRole::RetrievalQuery);
    }

    #[test]
    fn test_retrieval_result_from_chunk_count_field() {
        let mut fields = HashMap::new();
        fields.insert("chunk_count".to_string(), Value::Number(3.into()));
        let e = make_entity_with_fields(
            EntityType::RetrievalEvent,
            r#"{"msg":"Retrieval augmented context built","chunk_count":3}"#,
            fields,
        );
        assert_eq!(assign_role(&e), SemanticRole::RetrievalResult);
    }

    #[test]
    fn test_retrieval_query_from_raw_text() {
        let e = make_entity(
            EntityType::RetrievalEvent,
            "vector_store similarity_search query=weather k=5",
        );
        assert_eq!(assign_role(&e), SemanticRole::RetrievalQuery);
    }

    #[test]
    fn test_retrieval_result_from_raw_text() {
        let e = make_entity(
            EntityType::RetrievalEvent,
            "Retrieved 3 document chunks from vector store",
        );
        assert_eq!(assign_role(&e), SemanticRole::RetrievalResult);
    }

    #[test]
    fn test_retrieval_defaults_to_query() {
        let e = make_entity(
            EntityType::RetrievalEvent,
            "RAG pipeline activated",
        );
        assert_eq!(assign_role(&e), SemanticRole::RetrievalQuery);
    }

    // ── AgentStep disambiguation ──────────────────────────────────────────────

    #[test]
    fn test_agent_step_thought_is_reasoning() {
        let e = make_entity(
            EntityType::AgentStep,
            "Thought: I need to search for the current weather.",
        );
        assert_eq!(assign_role(&e), SemanticRole::AgentReasoning);
    }

    #[test]
    fn test_agent_step_action_is_agent_action() {
        let e = make_entity(EntityType::AgentStep, "Action: web_search");
        assert_eq!(assign_role(&e), SemanticRole::AgentAction);
    }

    #[test]
    fn test_agent_step_observation_is_agent_observation() {
        let e = make_entity(
            EntityType::AgentStep,
            "Observation: The weather is 72°F.",
        );
        assert_eq!(assign_role(&e), SemanticRole::AgentObservation);
    }

    #[test]
    fn test_agent_step_final_answer_is_reasoning() {
        let e = make_entity(
            EntityType::AgentStep,
            "Final Answer: The weather in NYC is 72°F.",
        );
        assert_eq!(assign_role(&e), SemanticRole::AgentReasoning);
    }

    #[test]
    fn test_agent_executor_is_reasoning() {
        let e = make_entity(
            EntityType::AgentStep,
            r#"{"msg":"AgentExecutor running","tool_count":3}"#,
        );
        assert_eq!(assign_role(&e), SemanticRole::AgentReasoning);
    }

    // ── McpEvent disambiguation ───────────────────────────────────────────────

    #[test]
    fn test_mcp_request_has_method_field() {
        let mut fields = HashMap::new();
        fields.insert("method".to_string(), Value::String("initialize".to_string()));
        let e = make_entity_with_fields(
            EntityType::McpEvent,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            fields,
        );
        assert_eq!(assign_role(&e), SemanticRole::McpRequest);
    }

    #[test]
    fn test_mcp_response_has_result_in_raw() {
        let e = make_entity(
            EntityType::McpEvent,
            r#"{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"mcp"}}}"#,
        );
        assert_eq!(assign_role(&e), SemanticRole::McpResponse);
    }

    #[test]
    fn test_mcp_error_response() {
        let e = make_entity(
            EntityType::McpEvent,
            r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"Method not found"}}"#,
        );
        assert_eq!(assign_role(&e), SemanticRole::McpResponse);
    }

    #[test]
    fn test_mcp_no_signals_defaults_to_request() {
        let e = make_entity(
            EntityType::McpEvent,
            r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{}}"#,
        );
        assert_eq!(assign_role(&e), SemanticRole::McpRequest);
    }

    // ── Idempotency ───────────────────────────────────────────────────────────

    #[test]
    fn test_classify_is_idempotent() {
        let mut entities = vec![make_entity(
            EntityType::CompletionEvent,
            r#"{"finish_reason":"stop"}"#,
        )];
        classify(&mut entities);
        assert_eq!(entities[0].semantic_role, SemanticRole::AssistantTurn);
        // Pre-assign a non-Unknown role and confirm it is not overwritten
        entities[0].semantic_role = SemanticRole::UserTurn;
        classify(&mut entities);
        assert_eq!(
            entities[0].semantic_role,
            SemanticRole::UserTurn,
            "classify must not overwrite an already-assigned role"
        );
    }

    // ── Integration: classify after entity_extractor::extract() ──────────────

    fn extract_and_classify(
        content: &str,
        format: &LogFormat,
    ) -> Vec<EntityRecord> {
        let agentic = empty_agentic();
        let mut entities =
            entity_extractor::extract(content, format, &None, &agentic, "h", "t", "trace1");
        classify(&mut entities);
        entities
    }

    #[test]
    fn test_openai_fixture_roles() {
        let content = fixture("openai_chat_completions.log");
        let entities = extract_and_classify(&content, &json_format());

        let roles: Vec<SemanticRole> = entities.iter().map(|e| e.semantic_role.clone()).collect();
        assert!(roles.contains(&SemanticRole::SystemPrompt), "expected SystemPrompt: {roles:?}");
        assert!(roles.contains(&SemanticRole::UserTurn), "expected UserTurn: {roles:?}");
        assert!(roles.contains(&SemanticRole::ToolInvocation), "expected ToolInvocation: {roles:?}");
        assert!(roles.contains(&SemanticRole::ToolResponse), "expected ToolResponse: {roles:?}");
        assert!(roles.contains(&SemanticRole::AssistantTurn), "expected AssistantTurn: {roles:?}");
        assert!(roles.contains(&SemanticRole::ContextAssembly), "expected ContextAssembly: {roles:?}");
        assert!(
            roles.contains(&SemanticRole::RetrievalQuery)
                || roles.contains(&SemanticRole::RetrievalResult),
            "expected a RetrievalQuery or RetrievalResult: {roles:?}"
        );
    }

    #[test]
    fn test_react_fixture_roles() {
        let content = fixture("react_agent.log");
        let entities = extract_and_classify(&content, &plain_format());

        let roles: Vec<SemanticRole> = entities.iter().map(|e| e.semantic_role.clone()).collect();
        assert!(
            roles.contains(&SemanticRole::AgentReasoning),
            "expected AgentReasoning: {roles:?}"
        );
        assert!(
            roles.contains(&SemanticRole::ToolInvocation),
            "expected ToolInvocation: {roles:?}"
        );
        assert!(
            roles.contains(&SemanticRole::ToolResponse),
            "expected ToolResponse: {roles:?}"
        );
    }

    #[test]
    fn test_mcp_fixture_roles() {
        let content = fixture("mcp_jsonrpc.log");
        let entities = extract_and_classify(&content, &json_format());

        let roles: Vec<SemanticRole> = entities.iter().map(|e| e.semantic_role.clone()).collect();
        assert!(roles.contains(&SemanticRole::McpRequest), "expected McpRequest: {roles:?}");
        assert!(roles.contains(&SemanticRole::McpResponse), "expected McpResponse: {roles:?}");
    }

    #[test]
    fn test_langchain_fixture_roles() {
        let content = fixture("langchain_json.log");
        let entities = extract_and_classify(&content, &json_format());

        let roles: Vec<SemanticRole> = entities.iter().map(|e| e.semantic_role.clone()).collect();
        assert!(roles.contains(&SemanticRole::AgentReasoning), "{roles:?}");
        assert!(roles.contains(&SemanticRole::ToolInvocation), "{roles:?}");
        assert!(roles.contains(&SemanticRole::ToolResponse), "{roles:?}");
        assert!(roles.contains(&SemanticRole::AssistantTurn), "{roles:?}");
    }

    #[test]
    fn test_crewai_fixture_roles() {
        let content = fixture("crewai_logfmt.log");
        let entities = extract_and_classify(&content, &logfmt_format());

        let roles: Vec<SemanticRole> = entities.iter().map(|e| e.semantic_role.clone()).collect();
        assert!(roles.contains(&SemanticRole::AgentReasoning), "{roles:?}");
        assert!(roles.contains(&SemanticRole::ToolInvocation), "{roles:?}");
        assert!(roles.contains(&SemanticRole::ToolResponse), "{roles:?}");
        assert!(roles.contains(&SemanticRole::AssistantTurn), "{roles:?}");
    }

    #[test]
    fn test_no_unknown_roles_in_openai_fixture() {
        let content = fixture("openai_chat_completions.log");
        let entities = extract_and_classify(&content, &json_format());

        let unknowns: Vec<&EntityRecord> =
            entities.iter().filter(|e| e.semantic_role == SemanticRole::Unknown).collect();
        assert!(
            unknowns.is_empty(),
            "expected no Unknown roles in openai fixture, got {} with types: {:?}",
            unknowns.len(),
            unknowns.iter().map(|e| (&e.entity_type, e.raw_text.get(..40).unwrap_or(&e.raw_text))).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_no_unknown_roles_in_mcp_fixture() {
        let content = fixture("mcp_jsonrpc.log");
        let entities = extract_and_classify(&content, &json_format());

        let unknowns: Vec<&EntityRecord> =
            entities.iter().filter(|e| e.semantic_role == SemanticRole::Unknown).collect();
        assert!(
            unknowns.is_empty(),
            "expected no Unknown roles in mcp fixture, got {} entities: {:?}",
            unknowns.len(),
            unknowns.iter().map(|e| e.raw_text.get(..40).unwrap_or(&e.raw_text)).collect::<Vec<_>>()
        );
    }
}
