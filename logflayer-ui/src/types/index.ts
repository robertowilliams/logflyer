export interface Target {
  id: string
  target_id: string
  status: 'active' | 'inactive' | string
  host?: string
  hostname?: string
  server?: string
  port?: number
  username?: string
  user?: string
  password?: string
  private_key?: string
  private_key_path?: string
  private_key_passphrase?: string
  log_paths?: string[]
  log_dirs?: string[]
  connection?: Record<string, unknown>
  credentials?: Record<string, unknown>
  /** How many lines to sample per file. Overrides global SAMPLE_LINE_COUNT. */
  sample_line_count?: number
  /** Max files to discover per log directory. Overrides global REMOTE_MAX_FILES_PER_TARGET. */
  max_files?: number
  [key: string]: unknown
}

export interface SampleRecord {
  id?: string
  timestamp: string
  target_id: string
  source_file: string
  sample_content: string
  host: string
  path: string
  sampling_mode: string
  line_count?: number
  file_size_bytes?: number
  processing_status: string
  error_details?: string
  sample_hash: string
}

export interface DeletionRecord {
  event: string
  sample_hash: string
  target_id: string
  reason: string
  deleted_at: string
}

export interface LogLine {
  raw: string
  level?: string
  timestamp?: string
  message?: string
}

export interface TrackingRecord {
  id?: string
  timestamp?: string
  level?: string
  message?: string
  [key: string]: unknown
}

export interface HealthResponse {
  status: 'healthy' | 'degraded'
  mongodb: string
  version: string
}

export interface PagedResponse<T> {
  records: T[]
  total: number
  page: number
  limit: number
}

export interface TargetsResponse {
  targets: Target[]
  total: number
}

export interface AdminSettings {
  // MongoDB
  mongodb_uri?: string
  source_db_name?: string
  source_collection_name?: string
  destination_db_name?: string
  tracking_db_name?: string
  tracking_collection_name?: string
  // Sampling
  sample_mode?: string
  sample_line_count?: number
  // Service
  run_mode?: string
  poll_interval_secs?: number
  concurrency?: number
  ssh_timeout_secs?: number
  api_port?: number
  // Discovery
  remote_max_depth?: number
  remote_max_files_per_target?: number
  remote_find_patterns?: string
  // Preprocessing
  preprocessing_enabled?: boolean
  preprocessing_agentic_threshold?: number
  preprocessing_max_schema_lines?: number
  metrics_port?: number
  // Classification
  classification_enabled?: boolean
  anthropic_api_key?: string
  classification_model?: string
  classification_signal_threshold?: number
  classification_max_per_cycle?: number
  classification_max_output_tokens?: number
  classification_api_base_url?: string
  classification_api_format?: string
  // Notifications
  notification_enabled?: boolean
  notification_severity_threshold?: string
  slack_webhook_url?: string
  webhook_url?: string
  webhook_secret?: string
  // Logging
  log_level?: string
  log_directory?: string
  log_file_base_name?: string
  log_max_file_size_bytes?: number
  log_max_files?: number
  // Config history
  config_history_enabled?: boolean
  config_history_master_key?: string
  config_history_key_id?: string
  config_history_collection_name?: string
}

export interface SettingsResponse {
  settings: AdminSettings
  has_overrides: boolean
  /** True when the running process started with an unconfirmed pending config */
  pending_confirmation: boolean
}

export interface HistoryEntry {
  version:    number
  created_at: string
  created_by: string
  source:     string
  reason:     string
  key_id:     string
}

export interface Finding {
  pattern:  string
  count:    number
  severity: string
  example:  string
}

// ── UpsideGate / Preprocessing types ─────────────────────────────────────────
// These mirror the backend Rust enums/structs in `logflayer/src/models.rs`,
// `preprocessing/prov_linker.rs`, and `preprocessing/otel_builder.rs`.  The
// vocabularies below are the canonical strings emitted by serde — keep them
// in lockstep with the Rust definitions or the views will silently render
// wrong colours / blank cells.

/**
 * Backend `models::EntityType` — `#[serde(rename_all = "snake_case")]`.
 *
 * These were previously declared PascalCase, matching the Rust *variant* names
 * rather than the wire format. Nothing crashed, it just silently never matched:
 * the Entities type filter selected nothing, every type badge fell through to the
 * default colour, and `entityTypeToSpanKind` returned `INTERNAL` for everything.
 */
export type EntityType =
  | 'prompt_event' | 'completion_event' | 'tool_call_event' | 'tool_result_event'
  | 'retrieval_event' | 'agent_step' | 'mcp_event' | 'context_window' | 'unknown'

/** Backend `models::SemanticRole` — `#[serde(rename_all = "snake_case")]`. */
export type SemanticRole =
  | 'system_prompt' | 'user_turn' | 'assistant_turn'
  | 'tool_invocation' | 'tool_response'
  | 'retrieval_query' | 'retrieval_result'
  | 'agent_reasoning' | 'agent_action' | 'agent_observation'
  | 'mcp_request' | 'mcp_response'
  | 'context_assembly' | 'memory_read' | 'memory_write'
  | 'unknown'

/** Backend `models::RelationType` — `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]`. */
export type RelationType =
  | 'TRIGGERED_BY' | 'GENERATED' | 'INFORMED' | 'FOLLOWED_BY'
  | 'RESPONDED_TO' | 'ASSEMBLED_FROM' | 'PART_OF' | 'DELEGATED_TO'

/** Backend `models::RelationSource` — `#[serde(rename_all = "snake_case")]`. */
/** Backend `models::RelationSource` — `#[serde(rename_all = "snake_case")]`.
 *  `inferred` is the `#[default]` and the source of most rules; it was missing. */
export type RelationSource = 'explicit' | 'inferred' | 'parsed'

/** Backend `prov_linker::ProvPredicate` — `#[serde(rename_all = "camelCase")]`. */
export type ProvPredicate =
  | 'wasGeneratedBy' | 'wasAttributedTo' | 'wasDerivedFrom'
  | 'used' | 'actedOnBehalfOf'

/** Backend `otel_builder::SpanKind` — `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]`. */
export type SpanKind =
  | 'INTERNAL' | 'CLIENT' | 'SERVER' | 'PRODUCER' | 'CONSUMER'

/** Backend `otel_builder::StatusCode` — `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]`. */
export type SpanStatusCode = 'UNSET' | 'OK' | 'ERROR'

/** Backend `models::ClassificationStatus` — `#[serde(rename_all = "snake_case")]`. */
export type ClassificationStatus =
  | 'pending' | 'classified' | 'skipped' | 'failed'

/** Mirror of `models::EntityRecord` — fields use Rust-default snake_case. */
export interface EntityRecord {
  entity_id:                 string
  entity_type:               EntityType
  semantic_role:             SemanticRole
  sample_hash:               string
  target_id:                 string
  /** OTel-compatible 16-byte hex trace id shared across a sample. */
  trace_id:                  string
  /** OTel-compatible 8-byte hex span id unique to this entity. */
  span_id:                   string
  parent_span_id?:           string | null
  prov_entity_id:            string
  prov_activity_id:          string
  /** Zero-based line position within the sample content. */
  line_index:                number
  /** The original log line(s) that produced this entity. */
  raw_text:                  string
  extracted_fields:          Record<string, unknown>
  model_id?:                 string | null
  tool_name?:                string | null
  mcp_server_id?:            string | null
  token_count?:              number | null
  latency_ms?:               number | null
  timestamp_utc?:            string | null
  content_embedding_id?:     string | null
  behavioral_embedding_id?:  string | null
}

/** Mirror of `models::RelationEdge`. */
export interface RelationEdge {
  relation_id:      string
  relation_type:    RelationType
  source_entity_id: string
  target_entity_id: string
  sample_hash:      string
  /** `1.0` = explicit field match; `0.7` = positional inference. */
  confidence:       number
  source:           RelationSource
  created_at:       string
}

// ─── Graph traversal (GET /api/v1/graph/*) ───────────────────────────────────

/** Which way `GET /api/v1/graph/{downstream,upstream}` walked the edge set. */
export type GraphDirection = 'downstream' | 'upstream'

/**
 * Response of `GET /api/v1/graph/downstream|upstream/:entity_id`.
 *
 * `edges` + `entities` are deliberately shaped to match `RelationGraph`'s
 * props, so a traversal result can be handed to the component directly.
 */
export interface GraphTraversal {
  /** The entity the walk started from, with any `ug:entity:` prefix stripped. */
  root:          string
  direction:     GraphDirection
  /** Levels actually walked — lower than the requested depth when the
   *  frontier emptied first. */
  depth_reached: number
  edges:         RelationEdge[]
  entities:      EntityRecord[]
  node_ids:      string[]
  node_count:    number
  edge_count:    number
  /** Visited ids with no entity record behind them. Normally empty — non-empty
   *  means an edge points somewhere that is not an entity. */
  unresolved_node_ids: string[]
  /** True when the server's node budget stopped the walk early. */
  truncated:     boolean
}

/** One hop of a resolved path, mirroring `graph_query::PathHop`. */
export interface PathHop {
  relation_id: string
  from:        string
  to:          string
}

/**
 * Response of `GET /api/v1/graph/path`.
 *
 * An unreachable pair returns `200` with `found: false` — it is a legitimate
 * answer rather than an error.
 */
export interface GraphPath {
  found:     boolean
  /** True when the search hit a server budget. With `found: false` this means
   *  "we stopped looking", not "there is no path" — the two are different
   *  claims and must not be collapsed. */
  truncated: boolean
  from:      string
  to:        string
  hops:      PathHop[]
  hop_count: number
  edges:     RelationEdge[]
  entities:  EntityRecord[]
  node_ids:  string[]
}

/** Mirror of `prov_linker::ProvTriple`. */
export interface ProvTriple {
  subject:     string
  predicate:   ProvPredicate
  object:      string
  sample_hash: string
  created_at:  string
}

/** Mirror of `otel_builder::SpanStatus`. */
export interface OtelSpanStatus {
  code:    SpanStatusCode
  /** Skipped during serialisation when empty, so it may be absent. */
  message?: string
}

/** Mirror of `otel_builder::OtelSpan`. */
export interface OtelSpan {
  trace_id:              string
  span_id:               string
  parent_span_id?:       string | null
  name:                  string
  kind:                  SpanKind
  start_time_unix_nano:  number
  end_time_unix_nano:    number
  /** Backend uses a typed enum (`AttributeValue`) — string|number|boolean here. */
  attributes:            Record<string, string | number | boolean>
  status:                OtelSpanStatus
  sample_hash:           string
}

/** Mirror of `models::LogFormat` (excerpt — the parts the views reference). */
export interface FormatInfo {
  log_type:          string
  timestamp_field?:  string | null
  level_field?:      string | null
  message_field?:    string | null
  timestamp_format?: string | null
  multiline?:        boolean
}

/** Mirror of `models::AgenticScan`. */
export interface AgenticScan {
  signal_score:        number
  worth_classifying:   boolean
  detected_frameworks: string[]
  matched_patterns:    string[]
  agentic_line_count:  number
}

/** Mirror of `models::SampleStats`. */
export interface SampleStats {
  total_lines:        number
  non_empty_lines:    number
  empty_line_ratio:   number
  avg_line_length:    number
  time_span_secs?:    number | null
  level_distribution: Record<string, number>
  unique_line_ratio:  number
}

/** Mirror of `models::IngestionHints`. */
export interface IngestionHints {
  prompt_template:      unknown
  suggested_chunk_size: number
  worth_classifying:    boolean
  skip_reason?:         string | null
  priority:             number
}

/** Mirror of `models::SampleMetadata`. */
export interface SampleMetadata {
  sample_hash:           string
  target_id:             string
  analyzed_at:           string
  preprocessing_version: string
  format:                FormatInfo
  stats:                 SampleStats
  agentic_scan:          AgenticScan
  schema?:               unknown | null
  ingestion_hints:       IngestionHints
  classification_status: ClassificationStatus
  otel_trace_id:         string
  entities:              EntityRecord[]
  relations:             RelationEdge[]
  entity_count:          number
  relation_count:        number
}

export interface ClassificationRecord {
  id?:                    string
  sample_hash:            string
  target_id:              string
  classified_at:          string
  model:                  string
  severity:               'critical' | 'warning' | 'info' | 'normal'
  categories:             string[]
  summary:                string
  key_findings:           Finding[]
  recommendations:        string[]
  confidence:             number
  input_tokens:           number
  output_tokens:          number
  classification_version: string
}
