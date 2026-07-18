//! Stage 6 supplemental — dedicated MCP JSON-RPC 2.0 parser.
//!
//! The general-purpose field extractor in [`super::entity_extractor`] handles
//! all entity types with a single pass of regexes and JSON field lookups.
//! For [`EntityType::McpEvent`] lines the generic pass misses several fields
//! that are only accessible through structured traversal of the JSON-RPC
//! envelope (e.g. `result.serverInfo.name`, `result.tools[*].name`,
//! `params.clientInfo`, `error.code`).
//!
//! This module provides [`parse`], which performs a deep, MCP-aware parse of
//! a single log line and returns an [`McpParsed`] value carrying all
//! extractable fields.  The entity extractor calls it after detecting a line
//! as `McpEvent` and merges the result with the generic extracted fields,
//! giving richer data to downstream stages.
//!
//! # JSON-RPC 2.0 message shapes
//!
//! | Shape         | Discriminants                              |
//! |---------------|--------------------------------------------|
//! | Request       | `method` present + `id` present            |
//! | Notification  | `method` present + `id` absent             |
//! | Response      | `result` present + `id` present            |
//! | ErrorResponse | `error` present + `id` present             |
//!
//! # MCP capability namespaces
//!
//! | Capability      | Method prefix / literal                    |
//! |-----------------|--------------------------------------------|
//! | `Lifecycle`     | `initialize`, `initialized`, `ping`        |
//! | `Tools`         | `tools/`                                   |
//! | `Resources`     | `resources/`                               |
//! | `Prompts`       | `prompts/`                                 |
//! | `Sampling`      | `sampling/`                                |
//! | `Notifications` | `notifications/`                           |

use serde_json::Value;

// ─── Public types ─────────────────────────────────────────────────────────────

/// Discriminated message type within the JSON-RPC 2.0 protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpMessageType {
    /// `{ "jsonrpc":"2.0", "id":<id>, "method":"…", "params":{…} }`
    Request,
    /// `{ "jsonrpc":"2.0", "method":"…", "params":{…} }` — no `id`
    Notification,
    /// `{ "jsonrpc":"2.0", "id":<id>, "result":{…} }`
    Response,
    /// `{ "jsonrpc":"2.0", "id":<id>, "error":{"code":…,"message":"…"} }`
    ErrorResponse,
}

/// Coarse-grained functional area within the MCP specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpCapability {
    /// `initialize` / `initialized` / `ping`
    Lifecycle,
    /// `tools/list` / `tools/call`
    Tools,
    /// `resources/list` / `resources/read` / `resources/subscribe` / …
    Resources,
    /// `prompts/list` / `prompts/get`
    Prompts,
    /// `sampling/createMessage`
    Sampling,
    /// `notifications/*`
    Notifications,
}

/// All fields extracted from a single MCP JSON-RPC 2.0 log line.
///
/// Optional fields are `None` when the information is not present in the
/// message (e.g. `tool_name` is `None` for a `tools/list` request).
#[derive(Debug, Clone)]
pub struct McpParsed {
    /// Discriminated message type.
    pub message_type: McpMessageType,

    /// JSON-RPC `id` — `Some` for requests/responses, `None` for notifications.
    /// Stored as a raw JSON [`Value`] because the spec allows `string | number | null`.
    pub id: Option<Value>,

    /// RPC method name, present on requests and notifications.
    /// `None` for response/error messages (method must be inferred from
    /// the matching request by the caller).
    pub method: Option<String>,

    /// High-level MCP capability area derived from `method`.
    pub capability: Option<McpCapability>,

    // ── Server / client identity ──────────────────────────────────────────────

    /// Server name extracted from `result.serverInfo.name` (initialize response)
    /// or `params.serverInfo.name`.
    pub server_id: Option<String>,

    /// Server version from `result.serverInfo.version`.
    pub server_version: Option<String>,

    /// Client name from `params.clientInfo.name`.
    pub client_id: Option<String>,

    /// MCP protocol version string (`params.protocolVersion` or
    /// `result.protocolVersion`).
    pub protocol_version: Option<String>,

    // ── Tool fields ───────────────────────────────────────────────────────────

    /// Tool being invoked — `params.name` on a `tools/call` request.
    pub tool_name: Option<String>,

    /// All tool names advertised in a `tools/list` response
    /// (`result.tools[*].name`).
    pub available_tools: Vec<String>,

    // ── Resource fields ───────────────────────────────────────────────────────

    /// URI targeted by a `resources/read` request (`params.uri`) or the first
    /// URI in a `resources/list` response.
    pub resource_uri: Option<String>,

    /// All resource URIs advertised in a `resources/list` response.
    pub available_resources: Vec<String>,

    // ── Prompt fields ─────────────────────────────────────────────────────────

    /// All prompt names advertised in a `prompts/list` response
    /// (`result.prompts[*].name`).
    pub available_prompts: Vec<String>,

    // ── Error fields ─────────────────────────────────────────────────────────

    /// Set to `true` for [`McpMessageType::ErrorResponse`] messages **and** for
    /// successful tool responses where `result.isError == true`.
    pub is_error: bool,

    /// JSON-RPC error code from `error.code`.
    pub error_code: Option<i64>,

    /// Human-readable error description from `error.message`.
    pub error_message: Option<String>,
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Parse `line` as a MCP JSON-RPC 2.0 message.
///
/// Returns `None` when:
/// * `line` is not valid JSON, or
/// * the parsed object does not contain `"jsonrpc":"2.0"`.
///
/// All extracted fields are best-effort; missing or malformed sub-fields
/// simply leave the corresponding `McpParsed` field as `None` / empty `Vec`.
pub fn parse(line: &str) -> Option<McpParsed> {
    let v: Value = serde_json::from_str(line).ok()?;
    let obj = v.as_object()?;

    // Must be a JSON-RPC 2.0 message
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return None;
    }

    let id     = obj.get("id").cloned();
    let method = obj.get("method").and_then(Value::as_str).map(str::to_string);
    let has_result = obj.contains_key("result");
    let has_error  = obj.contains_key("error");

    let message_type = classify_message(id.as_ref(), method.as_deref(), has_result, has_error);
    let capability   = method.as_deref().and_then(capability_from_method);

    // ── Server / client identity ──────────────────────────────────────────────
    let (server_id, server_version, client_id, protocol_version) =
        extract_identity(obj, method.as_deref());

    // ── Tool fields ───────────────────────────────────────────────────────────
    let tool_name       = extract_tool_name(obj, method.as_deref());
    let available_tools = extract_tool_list(obj);

    // ── Resource fields ───────────────────────────────────────────────────────
    let (resource_uri, available_resources) = extract_resources(obj, method.as_deref());

    // ── Prompt fields ─────────────────────────────────────────────────────────
    let available_prompts = extract_prompt_list(obj);

    // ── Error fields ─────────────────────────────────────────────────────────
    let (is_error, error_code, error_message) = extract_error(obj);

    Some(McpParsed {
        message_type,
        id,
        method,
        capability,
        server_id,
        server_version,
        client_id,
        protocol_version,
        tool_name,
        available_tools,
        resource_uri,
        available_resources,
        available_prompts,
        is_error,
        error_code,
        error_message,
    })
}

// ─── Message classification ───────────────────────────────────────────────────

fn classify_message(
    id:         Option<&Value>,
    method:     Option<&str>,
    has_result: bool,
    has_error:  bool,
) -> McpMessageType {
    if has_error && id.is_some() {
        McpMessageType::ErrorResponse
    } else if has_result && id.is_some() {
        McpMessageType::Response
    } else if method.is_some() && id.is_some() {
        McpMessageType::Request
    } else {
        // method present + no id, or degenerate message
        McpMessageType::Notification
    }
}

// ─── Capability classification ────────────────────────────────────────────────

fn capability_from_method(method: &str) -> Option<McpCapability> {
    if method.starts_with("tools/") {
        Some(McpCapability::Tools)
    } else if method.starts_with("resources/") {
        Some(McpCapability::Resources)
    } else if method.starts_with("prompts/") {
        Some(McpCapability::Prompts)
    } else if method.starts_with("sampling/") {
        Some(McpCapability::Sampling)
    } else if method.starts_with("notifications/") {
        Some(McpCapability::Notifications)
    } else if matches!(method, "initialize" | "initialized" | "ping") {
        Some(McpCapability::Lifecycle)
    } else {
        None
    }
}

// ─── Identity extraction ──────────────────────────────────────────────────────

/// Returns `(server_id, server_version, client_id, protocol_version)`.
fn extract_identity(
    obj:    &serde_json::Map<String, Value>,
    method: Option<&str>,
) -> (Option<String>, Option<String>, Option<String>, Option<String>) {
    let params = obj.get("params").and_then(Value::as_object);
    let result = obj.get("result").and_then(Value::as_object);

    // Server identity — initialize response: result.serverInfo.{name,version}
    let (server_id, server_version) = {
        let info = result
            .and_then(|r| r.get("serverInfo"))
            .and_then(Value::as_object)
            .or_else(|| params.and_then(|p| p.get("serverInfo")).and_then(Value::as_object));
        let name    = info.and_then(|i| i.get("name")).and_then(Value::as_str).map(str::to_string);
        let version = info.and_then(|i| i.get("version")).and_then(Value::as_str).map(str::to_string);
        (name, version)
    };

    // Client identity — initialize request: params.clientInfo.name
    let client_id = params
        .and_then(|p| p.get("clientInfo"))
        .and_then(Value::as_object)
        .and_then(|c| c.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string);

    // Protocol version — params.protocolVersion (request) or result.protocolVersion (response)
    let protocol_version = params
        .and_then(|p| p.get("protocolVersion"))
        .or_else(|| result.and_then(|r| r.get("protocolVersion")))
        .and_then(Value::as_str)
        .map(str::to_string);

    // Fallback: server_id from a direct server_id/mcp_server field (non-spec logs)
    let server_id = server_id.or_else(|| {
        for key in &["server_id", "mcp_server", "serverId"] {
            if let Some(v) = obj.get(*key).and_then(Value::as_str) {
                return Some(v.to_string());
            }
        }
        // Try method-context: if it's an initialize request, also check params.serverInfo
        if method == Some("initialize") {
            params
                .and_then(|p| p.get("serverInfo"))
                .and_then(Value::as_object)
                .and_then(|i| i.get("name"))
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            None
        }
    });

    (server_id, server_version, client_id, protocol_version)
}

// ─── Tool extraction ──────────────────────────────────────────────────────────

/// Tool name from `params.name` on a `tools/call` request.
fn extract_tool_name(
    obj:    &serde_json::Map<String, Value>,
    method: Option<&str>,
) -> Option<String> {
    // Explicit tools/call: params.name
    if method == Some("tools/call") {
        let name = obj
            .get("params")
            .and_then(Value::as_object)
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if name.is_some() {
            return name;
        }
    }

    // Generic fallback: flat params.name or top-level name for any tools/* method
    if method.map_or(false, |m| m.starts_with("tools/")) {
        obj.get("params")
            .and_then(Value::as_object)
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string)
    } else {
        None
    }
}

/// List of tool names from a `tools/list` response: `result.tools[*].name`.
fn extract_tool_list(obj: &serde_json::Map<String, Value>) -> Vec<String> {
    obj.get("result")
        .and_then(Value::as_object)
        .and_then(|r| r.get("tools"))
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

// ─── Resource extraction ──────────────────────────────────────────────────────

/// Returns `(primary_uri, all_uris)`.
fn extract_resources(
    obj:    &serde_json::Map<String, Value>,
    method: Option<&str>,
) -> (Option<String>, Vec<String>) {
    // resources/read request: params.uri
    if method == Some("resources/read") || method == Some("resources/subscribe") {
        let uri = obj
            .get("params")
            .and_then(Value::as_object)
            .and_then(|p| p.get("uri"))
            .and_then(Value::as_str)
            .map(str::to_string);
        return (uri, vec![]);
    }

    // resources/list response: result.resources[*].uri
    let uris: Vec<String> = obj
        .get("result")
        .and_then(Value::as_object)
        .and_then(|r| r.get("resources"))
        .and_then(Value::as_array)
        .map(|res| {
            res.iter()
                .filter_map(|r| r.get("uri").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let primary = uris.first().cloned();
    (primary, uris)
}

// ─── Prompt extraction ────────────────────────────────────────────────────────

/// List of prompt names from a `prompts/list` response: `result.prompts[*].name`.
fn extract_prompt_list(obj: &serde_json::Map<String, Value>) -> Vec<String> {
    obj.get("result")
        .and_then(Value::as_object)
        .and_then(|r| r.get("prompts"))
        .and_then(Value::as_array)
        .map(|prompts| {
            prompts
                .iter()
                .filter_map(|p| p.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

// ─── Error extraction ─────────────────────────────────────────────────────────

/// Returns `(is_error, error_code, error_message)`.
///
/// `is_error` is `true` for JSON-RPC error responses **and** for tool responses
/// where `result.isError == true` (MCP convention for tool-level failures).
fn extract_error(obj: &serde_json::Map<String, Value>) -> (bool, Option<i64>, Option<String>) {
    // JSON-RPC error envelope: { "error": { "code": -32601, "message": "…" } }
    if let Some(err) = obj.get("error").and_then(Value::as_object) {
        let code    = err.get("code").and_then(Value::as_i64);
        let message = err.get("message").and_then(Value::as_str).map(str::to_string);
        return (true, code, message);
    }

    // MCP tool-level error: result.isError == true
    let tool_error = obj
        .get("result")
        .and_then(Value::as_object)
        .and_then(|r| r.get("isError"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    (tool_error, None, None)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn parse_ok(line: &str) -> McpParsed {
        parse(line).unwrap_or_else(|| panic!("parse returned None for: {line}"))
    }

    // ── Non-MCP lines return None ─────────────────────────────────────────────

    #[test]
    fn non_json_returns_none() {
        assert!(parse("not json").is_none());
    }

    #[test]
    fn wrong_jsonrpc_version_returns_none() {
        assert!(parse(r#"{"jsonrpc":"1.0","id":1,"method":"foo"}"#).is_none());
    }

    #[test]
    fn missing_jsonrpc_field_returns_none() {
        assert!(parse(r#"{"id":1,"method":"initialize","params":{}}"#).is_none());
    }

    #[test]
    fn empty_object_returns_none() {
        assert!(parse("{}").is_none());
    }

    // ── Message type classification ───────────────────────────────────────────

    #[test]
    fn initialize_request_is_request_type() {
        let msg = parse_ok(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
        assert_eq!(msg.message_type, McpMessageType::Request);
    }

    #[test]
    fn initialize_response_is_response_type() {
        let msg = parse_ok(r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"test-mcp","version":"1.0"}}}"#);
        assert_eq!(msg.message_type, McpMessageType::Response);
    }

    #[test]
    fn error_response_is_error_response_type() {
        let msg = parse_ok(r#"{"jsonrpc":"2.0","id":5,"error":{"code":-32601,"message":"Method not found"}}"#);
        assert_eq!(msg.message_type, McpMessageType::ErrorResponse);
    }

    #[test]
    fn notification_has_no_id() {
        // notifications/message — no id field
        let msg = parse_ok(r#"{"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":"ready"}}"#);
        assert_eq!(msg.message_type, McpMessageType::Notification);
        assert!(msg.id.is_none());
    }

    // ── id field ─────────────────────────────────────────────────────────────

    #[test]
    fn integer_id_is_preserved() {
        let msg = parse_ok(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_file","arguments":{}}}"#);
        assert_eq!(msg.id.as_ref().and_then(Value::as_i64), Some(3));
    }

    #[test]
    fn string_id_is_preserved() {
        let msg = parse_ok(r#"{"jsonrpc":"2.0","id":"req-abc","method":"ping","params":{}}"#);
        assert_eq!(msg.id.as_ref().and_then(Value::as_str), Some("req-abc"));
    }

    // ── method field ─────────────────────────────────────────────────────────

    #[test]
    fn method_present_on_request() {
        let msg = parse_ok(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#);
        assert_eq!(msg.method.as_deref(), Some("tools/list"));
    }

    #[test]
    fn method_absent_on_response() {
        let msg = parse_ok(r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}"#);
        assert!(msg.method.is_none());
    }

    // ── capability classification ─────────────────────────────────────────────

    #[test]
    fn initialize_is_lifecycle() {
        let msg = parse_ok(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
        assert_eq!(msg.capability, Some(McpCapability::Lifecycle));
    }

    #[test]
    fn ping_is_lifecycle() {
        let msg = parse_ok(r#"{"jsonrpc":"2.0","id":9,"method":"ping","params":{}}"#);
        assert_eq!(msg.capability, Some(McpCapability::Lifecycle));
    }

    #[test]
    fn tools_list_is_tools_capability() {
        let msg = parse_ok(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#);
        assert_eq!(msg.capability, Some(McpCapability::Tools));
    }

    #[test]
    fn tools_call_is_tools_capability() {
        let msg = parse_ok(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_file","arguments":{}}}"#);
        assert_eq!(msg.capability, Some(McpCapability::Tools));
    }

    #[test]
    fn resources_list_is_resources_capability() {
        let msg = parse_ok(r#"{"jsonrpc":"2.0","id":5,"method":"resources/list","params":{}}"#);
        assert_eq!(msg.capability, Some(McpCapability::Resources));
    }

    #[test]
    fn prompts_list_is_prompts_capability() {
        let msg = parse_ok(r#"{"jsonrpc":"2.0","id":6,"method":"prompts/list","params":{}}"#);
        assert_eq!(msg.capability, Some(McpCapability::Prompts));
    }

    #[test]
    fn sampling_create_message_is_sampling_capability() {
        let msg = parse_ok(r#"{"jsonrpc":"2.0","id":7,"method":"sampling/createMessage","params":{"messages":[]}}"#);
        assert_eq!(msg.capability, Some(McpCapability::Sampling));
    }

    #[test]
    fn notifications_method_is_notifications_capability() {
        let msg = parse_ok(r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed","params":{}}"#);
        assert_eq!(msg.capability, Some(McpCapability::Notifications));
    }

    #[test]
    fn response_has_no_capability_without_method() {
        let msg = parse_ok(r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}"#);
        assert!(msg.capability.is_none(), "response without method has no capability");
    }

    // ── Server / client identity ──────────────────────────────────────────────

    #[test]
    fn server_id_from_initialize_response() {
        let line = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"filesystem-mcp","version":"1.2.0"}}}"#;
        let msg = parse_ok(line);
        assert_eq!(msg.server_id.as_deref(), Some("filesystem-mcp"));
    }

    #[test]
    fn server_version_from_initialize_response() {
        let line = r#"{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"test-mcp","version":"2.0.1"}}}"#;
        let msg = parse_ok(line);
        assert_eq!(msg.server_version.as_deref(), Some("2.0.1"));
    }

    #[test]
    fn client_id_from_initialize_request() {
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","clientInfo":{"name":"claude-code","version":"1.0.0"}}}"#;
        let msg = parse_ok(line);
        assert_eq!(msg.client_id.as_deref(), Some("claude-code"));
    }

    #[test]
    fn protocol_version_from_request_params() {
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}"#;
        let msg = parse_ok(line);
        assert_eq!(msg.protocol_version.as_deref(), Some("2024-11-05"));
    }

    #[test]
    fn protocol_version_from_response_result() {
        let line = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"s","version":"1"}}}"#;
        let msg = parse_ok(line);
        assert_eq!(msg.protocol_version.as_deref(), Some("2024-11-05"));
    }

    // ── Tool fields ───────────────────────────────────────────────────────────

    #[test]
    fn tool_name_from_tools_call_request() {
        let line = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"/etc/hosts"}}}"#;
        let msg = parse_ok(line);
        assert_eq!(msg.tool_name.as_deref(), Some("read_file"));
    }

    #[test]
    fn tool_name_absent_for_tools_list_request() {
        let line = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let msg = parse_ok(line);
        assert!(msg.tool_name.is_none());
    }

    #[test]
    fn available_tools_from_tools_list_response() {
        let line = r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"read_file","description":"Read a file"},{"name":"write_file","description":"Write a file"},{"name":"list_directory","description":"List dir"}]}}"#;
        let msg = parse_ok(line);
        assert_eq!(msg.available_tools, vec!["read_file", "write_file", "list_directory"]);
    }

    #[test]
    fn available_tools_empty_for_non_list_response() {
        let line = r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"ok"}],"isError":false}}"#;
        let msg = parse_ok(line);
        assert!(msg.available_tools.is_empty());
    }

    // ── Resource fields ───────────────────────────────────────────────────────

    #[test]
    fn resource_uri_from_resources_read_request() {
        let line = r#"{"jsonrpc":"2.0","id":6,"method":"resources/read","params":{"uri":"file:///etc/hosts"}}"#;
        let msg = parse_ok(line);
        assert_eq!(msg.resource_uri.as_deref(), Some("file:///etc/hosts"));
    }

    #[test]
    fn available_resources_from_resources_list_response() {
        let line = r#"{"jsonrpc":"2.0","id":5,"result":{"resources":[{"uri":"file:///etc/hosts","name":"hosts","mimeType":"text/plain"},{"uri":"file:///etc/passwd","name":"passwd","mimeType":"text/plain"}]}}"#;
        let msg = parse_ok(line);
        assert_eq!(msg.available_resources, vec!["file:///etc/hosts", "file:///etc/passwd"]);
        assert_eq!(msg.resource_uri.as_deref(), Some("file:///etc/hosts"));
    }

    // ── Error fields ──────────────────────────────────────────────────────────

    #[test]
    fn jsonrpc_error_parsed_correctly() {
        let line = r#"{"jsonrpc":"2.0","id":5,"error":{"code":-32601,"message":"Method not found"}}"#;
        let msg = parse_ok(line);
        assert!(msg.is_error);
        assert_eq!(msg.error_code, Some(-32601));
        assert_eq!(msg.error_message.as_deref(), Some("Method not found"));
    }

    #[test]
    fn tool_level_error_via_is_error_flag() {
        let line = r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"Permission denied"}],"isError":true}}"#;
        let msg = parse_ok(line);
        assert!(msg.is_error);
        assert!(msg.error_code.is_none(), "tool-level error has no JSON-RPC code");
    }

    #[test]
    fn successful_response_is_not_error() {
        let line = r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"Hello"}],"isError":false}}"#;
        let msg = parse_ok(line);
        assert!(!msg.is_error);
    }

    // ── Fixture integration ───────────────────────────────────────────────────

    #[test]
    fn fixture_all_lines_parse_successfully() {
        let fixture = include_str!("../../tests/fixtures/mcp_jsonrpc.log");
        let failures: Vec<&str> = fixture
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter(|l| parse(l).is_none())
            .collect();
        assert!(failures.is_empty(), "failed to parse lines: {failures:?}");
    }

    #[test]
    fn fixture_initialize_request_has_client_id() {
        let fixture = include_str!("../../tests/fixtures/mcp_jsonrpc.log");
        let init_req = fixture
            .lines()
            .find(|l| l.contains(r#""method":"initialize""#))
            .expect("fixture must contain initialize request");
        let msg = parse_ok(init_req);
        assert_eq!(msg.client_id.as_deref(), Some("claude-code"));
    }

    #[test]
    fn fixture_initialize_response_has_server_info() {
        let fixture = include_str!("../../tests/fixtures/mcp_jsonrpc.log");
        let init_resp = fixture
            .lines()
            .filter(|l| !l.contains(r#""method""#))
            .find(|l| l.contains("serverInfo"))
            .expect("fixture must contain initialize response");
        let msg = parse_ok(init_resp);
        assert_eq!(msg.server_id.as_deref(),      Some("filesystem-mcp"));
        assert_eq!(msg.server_version.as_deref(), Some("1.2.0"));
        assert_eq!(msg.protocol_version.as_deref(), Some("2024-11-05"));
    }

    #[test]
    fn fixture_tools_list_response_has_all_tools() {
        let fixture = include_str!("../../tests/fixtures/mcp_jsonrpc.log");
        let resp = fixture
            .lines()
            .find(|l| l.contains(r#""tools":"#) && l.contains("description"))
            .expect("fixture must contain tools/list response");
        let msg = parse_ok(resp);
        assert_eq!(msg.available_tools.len(), 3);
        assert!(msg.available_tools.contains(&"read_file".to_string()));
        assert!(msg.available_tools.contains(&"write_file".to_string()));
        assert!(msg.available_tools.contains(&"list_directory".to_string()));
    }

    #[test]
    fn fixture_tools_call_requests_have_tool_name() {
        let fixture = include_str!("../../tests/fixtures/mcp_jsonrpc.log");
        let calls: Vec<McpParsed> = fixture
            .lines()
            .filter(|l| l.contains(r#""method":"tools/call""#))
            .map(parse_ok)
            .collect();
        assert_eq!(calls.len(), 2, "fixture has 2 tools/call requests");
        let names: Vec<&str> = calls.iter().filter_map(|m| m.tool_name.as_deref()).collect();
        assert!(names.contains(&"read_file"),  "expected read_file: {names:?}");
        assert!(names.contains(&"write_file"), "expected write_file: {names:?}");
    }

    #[test]
    fn fixture_resources_list_response_has_resource_uris() {
        let fixture = include_str!("../../tests/fixtures/mcp_jsonrpc.log");
        let resp = fixture
            .lines()
            .find(|l| l.contains("\"resources\"") && l.contains("uri"))
            .expect("fixture must contain resources/list response");
        let msg = parse_ok(resp);
        assert!(!msg.available_resources.is_empty());
        assert!(msg.available_resources[0].starts_with("file://"));
    }

    #[test]
    fn fixture_message_types_are_balanced() {
        let fixture = include_str!("../../tests/fixtures/mcp_jsonrpc.log");
        let msgs: Vec<McpParsed> = fixture
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(parse_ok)
            .collect();

        let requests  = msgs.iter().filter(|m| m.message_type == McpMessageType::Request).count();
        let responses = msgs.iter().filter(|m| m.message_type == McpMessageType::Response).count();
        assert_eq!(requests, responses,
            "fixture should have equal requests ({requests}) and responses ({responses})");
    }
}
