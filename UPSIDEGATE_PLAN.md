# UpsideGate Implementation Plan

**Project:** LogFlayer → UpsideGate ETL Extension  
**Status:** Planning  
**Version target:** preprocessing v2

---

## 1. Current State

The preprocessing pipeline in `src/preprocessing/` already has five working stages:

| Stage | Module | Output |
|---|---|---|
| 1 | `format_detector` | `LogFormat` — JSON / Logfmt / Syslog / Multiline / PlainText |
| 2 | `stats` | `SampleStats` — line counts, avg length, unique ratio, time span |
| 3 | `agentic_scanner` | `AgenticScan` — signal score, detected frameworks, matched patterns |
| 4 | `schema_extractor` | `LogSchema` — field names, types, presence ratios |
| 5 | `hints` | `IngestionHints` — prompt template, chunk size, priority |

These stages produce a `SampleMetadata` document stored in MongoDB. The downstream LLM classifier (`src/classification/`) consumes that document and routes samples with `signal_score >= threshold` to an LLM for coarse classification.

**What the pipeline does not yet do:**

- Parse log lines into typed entity records (PromptEvent, ToolCallEvent, etc.)
- Assign semantic roles to log events
- Build relationships between entities within a sample
- Produce W3C PROV identifiers or OTel-compatible span records
- Generate embeddings for vector search
- Write to a graph database or vector store
- Parse MCP protocol messages specifically

UpsideGate adds all of this as three additional pipeline stages plus two async output adapters.

---

## 2. Target Architecture

```
Raw log sample (SampleRecord)
        │
        ▼
┌─────────────────────────────────────────────────────┐
│                  Preprocessor::run()                │
│                                                     │
│  Stage 1  format_detector     →  LogFormat          │
│  Stage 2  stats               →  SampleStats        │
│  Stage 3  agentic_scanner     →  AgenticScan        │
│  Stage 4  schema_extractor    →  LogSchema          │
│  Stage 5  hints               →  IngestionHints     │
│  ──────────────────────────── NEW ──────────────    │
│  Stage 6  entity_extractor    →  Vec<EntityRecord>  │
│  Stage 7  semantic_classifier →  roles on entities  │
│  Stage 8  relation_extractor  →  Vec<RelationEdge>  │
└─────────────────────────────────────────────────────┘
        │
        ├── MongoDB: SampleMetadata (extended)
        │
        ├── [async] prov_linker + otel_builder
        │       → EntityBundle (PROV URIs, span records)
        │
        ├── [async] embedding_worker
        │       → content embeddings + behavioral embeddings
        │
        ├── [async] graph_writer
        │       → nodes + edges in graph DB
        │
        └── [async] vector_writer
                → embedded entity records in vector store
```

The three new synchronous stages (6-8) run inside the existing `spawn_blocking` call with the rest of the pipeline — no new thread overhead. The four async workers consume from MongoDB change streams or an internal channel after `Preprocessor::run()` returns.

---

## 3. New Data Models (`src/models.rs` additions)

### 3.1 Entity types

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    PromptEvent,
    CompletionEvent,
    ToolCallEvent,
    ToolResultEvent,
    RetrievalEvent,
    AgentStep,
    McpEvent,
    ContextWindow,
    Unknown,
}
```

### 3.2 Semantic roles

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRole {
    SystemPrompt,
    UserTurn,
    AssistantTurn,
    ToolInvocation,
    ToolResponse,
    RetrievalQuery,
    RetrievalResult,
    AgentReasoning,
    AgentAction,
    AgentObservation,
    McpRequest,
    McpResponse,
    ContextAssembly,
    MemoryRead,
    MemoryWrite,
    Unknown,
}
```

### 3.3 Entity record

```rust
pub struct EntityRecord {
    pub entity_id: String,            // UUID v4
    pub entity_type: EntityType,
    pub semantic_role: SemanticRole,
    pub sample_hash: String,          // FK to SampleRecord
    pub target_id: String,
    pub span_id: String,              // OTel span id (hex)
    pub trace_id: String,             // OTel trace id (hex)
    pub parent_span_id: Option<String>,
    pub prov_entity_id: String,       // URI: ug:entity:{entity_id}
    pub prov_activity_id: String,     // URI: ug:activity:{sample_hash}:{index}
    pub line_index: usize,            // position in the sample
    pub raw_text: String,             // the original log line(s)
    pub extracted_fields: HashMap<String, serde_json::Value>,
    pub model_id: Option<String>,
    pub tool_name: Option<String>,
    pub mcp_server_id: Option<String>,
    pub token_count: Option<u32>,
    pub latency_ms: Option<u64>,
    pub timestamp_utc: Option<DateTime>,
    pub content_embedding_id: Option<String>,   // set async by embedding_worker
    pub behavioral_embedding_id: Option<String>,
}
```

### 3.4 Relation edge

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RelationType {
    TriggeredBy,
    Generated,
    Informed,
    FollowedBy,
    RespondedTo,
    AssembledFrom,
    PartOf,
    DelegatedTo,
}

pub struct RelationEdge {
    pub relation_id: String,         // UUID v4
    pub relation_type: RelationType,
    pub source_entity_id: String,
    pub target_entity_id: String,
    pub sample_hash: String,
    pub confidence: f32,             // 1.0 = explicit, <1.0 = inferred
    pub source: RelationSource,      // Parsed | Inferred | Explicit
    pub created_at: DateTime,
}
```

### 3.5 Extended SampleMetadata

Add to the existing `SampleMetadata` struct:

```rust
pub entities: Vec<EntityRecord>,      // populated by stage 6-7
pub relations: Vec<RelationEdge>,     // populated by stage 8
pub entity_count: u32,
pub relation_count: u32,
pub otel_trace_id: String,           // assigned at pipeline entry
```

---

## 4. Phase Plan

### Phase 0 — Foundation (Week 1)

**Goal:** Unblock all subsequent phases. No functional change to the existing pipeline.

**Tasks:**

Add the new types from §3 to `src/models.rs`. Keep them behind a `#[cfg(feature = "upsidegate")]` feature flag initially so the existing pipeline compiles and tests pass unchanged.

Create empty module files:
- `src/preprocessing/entity_extractor.rs`
- `src/preprocessing/semantic_classifier.rs`
- `src/preprocessing/relation_extractor.rs`
- `src/preprocessing/prov_linker.rs`
- `src/preprocessing/otel_builder.rs`
- `src/embedding/mod.rs`
- `src/output/graph.rs`
- `src/output/vector.rs`

Add `otel_trace_id: String` to `SampleMetadata` and generate it with `uuid::Uuid::new_v4().simple().to_string()` at the top of `Preprocessor::run()`. This will be the stable identifier that links all downstream records.

Bump `PREPROCESSING_VERSION` to `"2"`. Add migration marker so the backfill job can identify v1 documents that lack entities/relations.

**Tests:** All existing tests must still pass.

---

### Phase 1 — Entity Extractor (Weeks 2–3)

**Goal:** Stage 6 — parse log lines into typed `EntityRecord` instances.

**Module:** `src/preprocessing/entity_extractor.rs`

**Interface:**

```rust
pub fn extract(
    content: &str,
    format: &LogFormat,
    schema: &Option<LogSchema>,
    agentic: &AgenticScan,
    sample_hash: &str,
    target_id: &str,
    trace_id: &str,
) -> Vec<EntityRecord>
```

**Implementation strategy:**

The extractor iterates non-empty lines. For each line it runs three passes:

Pass 1 — **Type detection.** Use the existing `AgenticScan.matched_patterns` as a first-pass signal, then apply entity-specific regex patterns:

| Entity type | Detection signal |
|---|---|
| `PromptEvent` | `"role": "user"`, `"role": "system"`, `system_message`, `HumanMessage`, `SystemMessage` |
| `CompletionEvent` | `finish_reason`, `"role": "assistant"`, `completion_tokens`, `AssistantMessage` |
| `ToolCallEvent` | `tool_call`, `function_call`, `tool_use`, `Action:` |
| `ToolResultEvent` | `tool_result`, `function_result`, `Observation:`, `Action Output:` |
| `RetrievalEvent` | `similarity_search`, `vector_store`, `retrieved`, `RAG`, `embedding` |
| `AgentStep` | `Thought:`, `PLAN`, `REFLECT`, `AgentExecutor`, `step=`, `crew.kickoff` |
| `McpEvent` | `mcp`, `MCP`, `"method":`, `"jsonrpc":` |
| `ContextWindow` | `context_window`, `assembled`, `token_limit`, `max_tokens` |

Pass 2 — **Field extraction.** For JSON lines, parse and extract known fields using the `LogSchema` as a guide. For Logfmt, split on `=`. For PlainText, extract via regex named groups. Store in `extracted_fields: HashMap<String, Value>`.

Pass 3 — **Span ID assignment.** Each entity gets its own `span_id`. Parent-child relationships are inferred from line order and entity type pairs (e.g., a `ToolResultEvent` following a `ToolCallEvent` on the same target gets `parent_span_id` pointing to the tool call).

**New test fixtures:** Add `tests/fixtures/mcp_jsonrpc.log`, `tests/fixtures/openai_chat_completions.log`, `tests/fixtures/react_agent.log`.

**Tests:** Unit tests for each entity type detector. Integration test verifying entity count and types for each existing fixture.

---

### Phase 2 — Semantic Classifier (Week 3)

**Goal:** Stage 7 — assign `SemanticRole` to each `EntityRecord`.

**Module:** `src/preprocessing/semantic_classifier.rs`

**Interface:**

```rust
pub fn classify(entities: &mut Vec<EntityRecord>)
```

Operates in place — enriches entities produced by Stage 6.

**Mapping logic:** Most `EntityType → SemanticRole` mappings are 1:1, but context disambiguates edge cases:

```
PromptEvent  +  role=system        →  SystemPrompt
PromptEvent  +  role=user          →  UserTurn
CompletionEvent                    →  AssistantTurn
ToolCallEvent                      →  ToolInvocation
ToolResultEvent                    →  ToolResponse
RetrievalEvent  +  query fields    →  RetrievalQuery
RetrievalEvent  +  result fields   →  RetrievalResult
AgentStep  +  "Thought"            →  AgentReasoning
AgentStep  +  "Action"             →  AgentAction
AgentStep  +  "Observation"        →  AgentObservation
McpEvent  +  request method        →  McpRequest
McpEvent  +  result/error          →  McpResponse
```

For ambiguous lines (e.g., a JSON blob that contains both prompt and completion data), assign `SemanticRole::Unknown` and set `confidence = 0.5` rather than guessing.

---

### Phase 3 — Relation Extractor (Weeks 4–5)

**Goal:** Stage 8 — build `RelationEdge` records between entities within a sample.

**Module:** `src/preprocessing/relation_extractor.rs`

**Interface:**

```rust
pub fn extract(
    entities: &[EntityRecord],
    sample_hash: &str,
) -> Vec<RelationEdge>
```

**Relation inference rules** (applied in order; first match wins):

```
PromptEvent → CompletionEvent (same turn or adjacent)     →  RespondedTo
AgentStep   → ToolCallEvent   (AgentStep precedes)        →  TriggeredBy
ToolCallEvent → ToolResultEvent (same tool_name, adjacent) →  Generated
RetrievalEvent → PromptEvent  (retrieval precedes prompt)  →  Informed
AgentStep(n) → AgentStep(n+1)                             →  FollowedBy
AgentStep   → McpEvent                                    →  DelegatedTo
Any entity  → ContextWindow (ContextWindow follows)       →  AssembledFrom
```

Edges that can be derived from explicit fields (e.g., a JSON `tool_call_id` that matches a later `tool_result`) get `confidence = 1.0`. Positionally inferred edges (based on line order and type pairs) get `confidence = 0.7`. Structurally implied edges (e.g., all entities are `PartOf` the same trace) get `confidence = 1.0`.

The `PartOf` relation is generated for every entity automatically: each entity gets a `PartOf` edge to the `otel_trace_id`.

---

### Phase 4 — PROV Linker + OTel Builder (Week 5)

**Goal:** Produce W3C PROV identifiers and OTel span records from the entity + relation graph.

**Modules:** `src/preprocessing/prov_linker.rs`, `src/preprocessing/otel_builder.rs`

These run as the first async step after `Preprocessor::run()` stores `SampleMetadata`. They consume entity and relation records and write back enriched records.

**PROV linker** assigns stable URIs following the pattern:
- Entities: `ug:entity:{entity_id}`
- Activities: `ug:activity:{sample_hash}:{entity_index}`
- Agents: `ug:agent:{model_id | target_id}`

It emits PROV relations as separate documents in a `prov_relations` MongoDB collection:
```
wasGeneratedBy(completion_entity_uri, inference_activity_uri)
used(inference_activity_uri, prompt_entity_uri)
wasDerivedFrom(tool_result_uri, tool_call_uri)
wasAssociatedWith(inference_activity_uri, model_agent_uri)
```

**OTel builder** assembles each `EntityRecord` into an OTel-compatible span JSON:
```json
{
  "traceId": "...",
  "spanId": "...",
  "parentSpanId": "...",
  "name": "{semantic_role}:{entity_type}",
  "startTimeUnixNano": "...",
  "endTimeUnixNano": "...",
  "attributes": {
    "ug.entity.type": "tool_call_event",
    "ug.semantic.role": "tool_invocation",
    "ug.tool.name": "web_search",
    "ug.model.id": "gpt-4o",
    "ug.sample.hash": "...",
    "ug.target.id": "..."
  }
}
```

Spans are stored in `otel_spans` collection and can be exported via a future OTLP endpoint.

---

### Phase 5 — Embedding Worker (Weeks 6–8)

**Goal:** Generate content and behavioral embeddings for each entity, stored in the vector database.

**Module:** `src/embedding/mod.rs`

**Design:** Runs as a tokio background task, consuming entity records from MongoDB via a change stream on `sample_metadata` where `entities` is present and `content_embedding_id` is null.

**Two embedding types:**

**Content embedding** — captures what the entity says. The input text is a normalized canonical form:
```
[{semantic_role}] [{entity_type}]: {raw_text truncated to 512 tokens}
```
This makes embeddings role-aware and comparable across heterogeneous log formats.

**Behavioral embedding** — captures what the entity does structurally. The input is a schema string encoding the entity's position in its relation graph:
```
type={entity_type} role={semantic_role} parent={parent_entity_type} 
children=[{child_entity_type}, ...] siblings=[{sibling_semantic_roles}]
```
This supports behavioral clustering queries: "find all agent runs with a similar tool-use pattern."

**Embedding provider config** (added to `AppConfig`):
```toml
[embedding]
enabled = true
provider = "openai"           # openai | local
model = "text-embedding-3-small"
api_key = ""                  # from env EMBEDDING_API_KEY
batch_size = 32
dimensions = 1536
local_model_path = ""         # for fastembed-rs
```

**MongoDB vector index** (Atlas Vector Search or self-hosted):
```json
{
  "type": "vectorSearch",
  "fields": [{
    "type": "vector",
    "path": "content_embedding",
    "numDimensions": 1536,
    "similarity": "cosine"
  }, {
    "type": "filter",
    "path": "entity_type"
  }, {
    "type": "filter", 
    "path": "semantic_role"
  }, {
    "type": "filter",
    "path": "target_id"
  }]
}
```

Two collections: `entity_content_embeddings`, `entity_behavioral_embeddings`. Each document stores the embedding vector, `entity_id`, and all filterable metadata fields.

---

### Phase 6 — Output Adapters (Weeks 8–11)

#### 6A — Graph Adapter (`src/output/graph.rs`)

**Short-term:** MongoDB-native property graph. Store entity nodes and relation edges in `entity_nodes` and `entity_edges` collections with full entity metadata and relation type. This is functional but not optimized for multi-hop traversal.

**Migration path:** Design the adapter behind a trait so switching to Neo4j requires only a new impl, not a rewrite:

```rust
pub trait GraphWriter: Send + Sync {
    async fn write_entities(&self, entities: &[EntityRecord]) -> Result<(), AppError>;
    async fn write_relations(&self, edges: &[RelationEdge]) -> Result<(), AppError>;
    async fn write_prov_triples(&self, triples: &[ProvTriple]) -> Result<(), AppError>;
}

pub struct MongoGraphWriter { ... }   // Phase 6 impl
pub struct Neo4jGraphWriter { ... }   // Future impl
```

When migrating to Neo4j, entity types map to node labels, relation types map to relationship types, and the PROV URIs become stable node identifiers.

Example Cypher for the lineage query "what produced this completion?":
```cypher
MATCH path = (c:CompletionEvent {entity_id: $id})<-[:RESPONDED_TO|GENERATED*1..5]-()
RETURN path
```

#### 6B — Vector Adapter (`src/output/vector.rs`)

Follows the same trait pattern:

```rust
pub trait VectorWriter: Send + Sync {
    async fn upsert_content_embedding(&self, record: ContentEmbeddingRecord) -> Result<(), AppError>;
    async fn upsert_behavioral_embedding(&self, record: BehavioralEmbeddingRecord) -> Result<(), AppError>;
    async fn similarity_search(&self, query_embedding: Vec<f32>, filters: EmbeddingFilter, k: usize) -> Result<Vec<SimilarityResult>, AppError>;
}

pub struct MongoVectorWriter { ... }    // Atlas Vector Search
pub struct QdrantVectorWriter { ... }   // Future impl
```

---

### Phase 7 — MCP Protocol Support (Weeks 9–10)

**Goal:** First-class parsing of MCP (Model Context Protocol) JSON-RPC messages.

**Module:** `src/preprocessing/mcp_parser.rs`

MCP messages follow JSON-RPC 2.0. The parser identifies and structures:

```
Request:      { "jsonrpc": "2.0", "id": N, "method": "...", "params": {...} }
Response:     { "jsonrpc": "2.0", "id": N, "result": {...} }
Notification: { "jsonrpc": "2.0", "method": "...", "params": {...} }
Error:        { "jsonrpc": "2.0", "id": N, "error": {"code": N, "message": "..."} }
```

Known MCP methods and their semantic role mappings:

| Method | Entity type | Semantic role |
|---|---|---|
| `initialize` | `McpEvent` | `McpRequest` |
| `resources/list` | `McpEvent` | `McpRequest` |
| `resources/read` | `McpEvent` | `McpRequest` |
| `tools/list` | `McpEvent` | `McpRequest` |
| `tools/call` | `McpEvent` | `ToolInvocation` |
| `prompts/get` | `McpEvent` | `McpRequest` |
| `sampling/createMessage` | `McpEvent` | `McpRequest` |
| (response to tools/call) | `McpEvent` | `ToolResponse` |

Add `mcp_server_id` extraction: the parser looks for `"serverId"`, `"server_id"`, or a URL pattern in the params.

Extend `agentic_scanner` with MCP-specific patterns (currently only catches `"method":` generically).

**New test fixture:** `tests/fixtures/mcp_session.log` — a multi-turn MCP session with initialize, tool calls, and responses.

---

## 5. Integration into Preprocessor::run()

The updated pipeline looks like this:

```rust
pub fn run(&self, sample_hash: &str, target_id: &str, content: &str) -> SampleMetadata {
    let trace_id = generate_trace_id();  // NEW: stable OTel trace id

    // Existing stages (unchanged)
    let format    = format_detector::detect(content);
    let stats     = stats::compute(content, &format.log_type);
    let agentic   = agentic_scanner::scan(content, self.config.agentic_threshold);
    let schema    = schema_extractor::extract(content, &format.log_type, self.config.max_schema_lines);
    let hints     = hints::derive(&format, &stats, &agentic);

    // NEW stages (UpsideGate)
    let (entities, relations) = if self.config.entity_extraction_enabled {
        let mut entities = entity_extractor::extract(
            content, &format, &schema, &agentic, sample_hash, target_id, &trace_id,
        );
        semantic_classifier::classify(&mut entities);
        let relations = relation_extractor::extract(&entities, sample_hash);
        (entities, relations)
    } else {
        (vec![], vec![])
    };

    SampleMetadata {
        sample_hash: sample_hash.to_string(),
        target_id: target_id.to_string(),
        otel_trace_id: trace_id,         // NEW
        analyzed_at: DateTime::now(),
        preprocessing_version: PREPROCESSING_VERSION.to_string(),
        format, stats, agentic, schema, hints,
        entities,                        // NEW
        relations,                       // NEW
        entity_count: entities.len() as u32,
        relation_count: relations.len() as u32,
        classification_status: ClassificationStatus::Pending,
    }
}
```

The new stages are gated by `entity_extraction_enabled` so they can be rolled out gradually without touching the existing signal-score / classification path.

---

## 6. Configuration Additions

Add to `AppConfig` / admin settings:

```
ENTITY_EXTRACTION_ENABLED=true
ENTITY_EXTRACTION_MIN_ENTITIES=1      # skip embedding/graph write if below this
EMBEDDING_ENABLED=false               # Phase 5
EMBEDDING_PROVIDER=openai
EMBEDDING_MODEL=text-embedding-3-small
EMBEDDING_API_KEY=
GRAPH_WRITER_ENABLED=false            # Phase 6A
GRAPH_WRITER_BACKEND=mongodb          # mongodb | neo4j
NEO4J_URI=
NEO4J_USERNAME=
NEO4J_PASSWORD=
VECTOR_WRITER_ENABLED=false           # Phase 6B
VECTOR_WRITER_BACKEND=mongodb         # mongodb | qdrant
QDRANT_URL=
QDRANT_API_KEY=
```

---

## 7. Backfill

After Phase 1 ships, a backfill subcommand processes existing v1 `SampleMetadata` documents:

```bash
logflayer backfill --reprocess-entities --batch-size 50
```

The existing `backfill.rs` / `backfill::purge_stale_metadata()` pattern already handles version-based reprocessing. Extend it to:

1. Find all documents with `preprocessing_version = "1"`
2. Re-fetch their `SampleRecord.sample_content` from MongoDB
3. Run the full v2 pipeline
4. Upsert the enriched `SampleMetadata`

---

## 8. Test Strategy

Each new module gets unit tests following the existing pattern (fixtures in `tests/fixtures/`):

| Module | Key tests |
|---|---|
| `entity_extractor` | One test per entity type using dedicated fixtures |
| `semantic_classifier` | Role assignment accuracy for ambiguous lines |
| `relation_extractor` | Relation count and types for a full ReAct agent session |
| `prov_linker` | URI format validation, triple completeness |
| `otel_builder` | Span JSON schema conformance |
| `embedding worker` | Batch size handling, error recovery, idempotency |
| `mcp_parser` | All four JSON-RPC message types, unknown methods |

Integration test: a full pipeline run on a synthetic MCP + LangChain combined log, asserting entity types, semantic roles, relation graph shape, and `SampleMetadata` completeness.

---

## 9. Delivery Milestones

| Week | Milestone | Dependencies |
|---|---|---|
| 1 | Phase 0: models, module skeletons, trace_id, version bump | None |
| 2–3 | Phase 1: entity extractor + fixtures | Phase 0 |
| 3 | Phase 2: semantic classifier | Phase 1 |
| 4–5 | Phase 3: relation extractor | Phase 1 |
| 5 | Phase 4: PROV linker + OTel builder | Phase 3 |
| 6–8 | Phase 5: embedding worker | Phase 1, embedding API access |
| 8–11 | Phase 6: graph + vector adapters | Phase 5 |
| 9–10 | Phase 7: MCP parser | Phase 1 |
| 11–12 | Backfill, integration tests, documentation | All phases |

Phases 5, 6, and 7 are partially independent — MCP parser (Phase 7) can proceed in parallel with embedding work (Phase 5) once Phase 1 is stable.

---

## 10. Trade-off Decisions

**Entity extraction is synchronous, embeddings are async.** The synchronous stages (6–8) add CPU work inside `spawn_blocking` — profiling on existing fixtures should confirm latency impact before shipping. Embeddings and graph writes are always async to avoid blocking the sampling loop.

**MongoDB first, specialized databases later.** Using MongoDB for both graph simulation and vector search in Phases 6A/6B lets you ship output adapters without new infrastructure. The trait boundary ensures switching to Neo4j or Qdrant is a configuration change, not a rewrite. Move when query patterns prove MongoDB's limitations.

**Confidence scores on inferred relations.** Rather than emitting only high-confidence edges, the relation extractor emits all candidates with scores. Consumers can filter by confidence threshold at query time, giving flexibility without losing data.

**`entity_extraction_enabled` flag.** The new stages are opt-in so the existing `signal_score → classification` path is unaffected during rollout. Once Phase 1 is validated, enable by default.
