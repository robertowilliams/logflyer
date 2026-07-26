# UpsideGate Implementation Plan

**Project:** LogFlayer → UpsideGate ETL Extension  
**Status:** ✅ **Shipped** — Phases 0–7 all landed. This document is now a
record of what was built and why, not a forward plan.  
**Version target:** preprocessing v2 — reached (`PREPROCESSING_VERSION = "2"`)

> **Read this first.** Everything below the phase plan was written *before* any
> of it shipped, so the future tense throughout is historical. Each phase now
> carries a `[shipped]` marker with the commit that landed it. Where the
> implementation diverged from the plan — and it did, in several places that
> matter — there is a **Divergence** note saying what changed and why. Those
> notes are the useful part of this document; the rest is archaeology.
>
> **Last verified against the code:** July 25, 2026.

---

## 1. Current State

> **Historical.** This section described the pre-UpsideGate pipeline. Stages 6–10
> and both output adapters now exist; see §4 for what landed. Kept because it
> records the starting point.

The preprocessing pipeline in `src/preprocessing/` already has five working stages:

| Stage | Module | Output |
|---|---|---|
| 1 | `format_detector` | `LogFormat` — JSON / Logfmt / Syslog / Multiline / PlainText |
| 2 | `stats` | `SampleStats` — line counts, avg length, unique ratio, time span |
| 3 | `agentic_scanner` | `AgenticScan` — signal score, detected frameworks, matched patterns |
| 4 | `schema_extractor` | `LogSchema` — field names, types, presence ratios |
| 5 | `hints` | `IngestionHints` — prompt template, chunk size, priority |

These stages produce a `SampleMetadata` document stored in MongoDB. The downstream LLM classifier (`src/classification/`) consumes that document and routes samples with `signal_score >= threshold` to an LLM for coarse classification.

**What the pipeline did not yet do** (all of this now exists):

- Parse log lines into typed entity records (PromptEvent, ToolCallEvent, etc.) — Phase 1
- Assign semantic roles to log events — Phase 2
- Build relationships between entities within a sample — Phase 3
- Produce W3C PROV identifiers or OTel-compatible span records — Phase 4
- Generate embeddings for vector search — Phase 5
- Write to a graph database or vector store — Phase 6
- Parse MCP protocol messages specifically — Phase 7

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

### Phase 0 — Foundation (Week 1) — `[shipped]` `5fb6f84`, `14ee734`

> **Divergence — no feature flag.** The types went into `src/models.rs`
> unconditionally; there is no `upsidegate` feature in `Cargo.toml`. The flag was
> meant to keep the old pipeline compiling, but the additive `#[serde(default)]`
> fields on `SampleMetadata` already guaranteed that, so the flag would have
> bought nothing and doubled the build matrix. Runtime kill switches
> (`ENTITY_EXTRACTION_ENABLED` and friends, §6) turned out to be what was
> actually wanted — you can disable a stage in production without a rebuild.
>
> **Divergence — trace ids are derived, not random.** The plan called for
> `uuid::Uuid::new_v4()`. Shipped as `ids::derive_trace_id`, a truncated
> SHA-256 of `(sample_hash, "trace")`. `uuid` is not a direct dependency of this
> crate — it is present only transitively, though the sibling `logflayersense`
> crate does depend on it directly.
> Random ids would have made the pipeline non-idempotent: re-processing a sample
> would mint a new trace and orphan every span written on the previous run.
> Deriving from `sample_hash` means re-runs reproduce the same ids and the output
> adapters' upsert filters actually match. Same reasoning applies to
> `derive_span_id`, `derive_entity_id` and `derive_relation_id`.

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

### Phase 1 — Entity Extractor (Weeks 2–3) — `[shipped]` `954b929`

> **Divergence — `schema` and `agentic` are accepted but unused.** They are
> `_schema` / `_agentic` in the signature. The plan wanted `AgenticScan.matched_patterns`
> as a first-pass signal and `LogSchema` to guide field extraction, but the
> prioritised regex set in `TYPE_PATTERNS` turned out to be sufficient on its own,
> and threading the schema through added coupling for no measured gain. The
> parameters were kept so the call site does not change if that turns out to be
> the wrong call.
>
> **Divergence — type detection is priority-scored, not first-match.** The
> highest-`priority` matching pattern wins rather than the first, because
> `"jsonrpc": "2.0"` and `finish_reason` can both appear on one line and
> first-match made the outcome depend on list order. (The loop short-circuits the
> regex for patterns that cannot beat the incumbent, so not every pattern is
> actually run — the outcome is the same.)
>
> **Later fix.** `timestamp_utc` was hardcoded `None` here and stayed that way
> until `1a3079b` (July 25) — the field existed and `otel_builder` read it, but
> nothing populated it, so every span carried a zero timestamp from July 17 to
>   July 25 2026. See
> Phase 4.

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

### Phase 2 — Semantic Classifier (Week 3) — `[shipped]` `954b929`

> **Divergence — no per-entity confidence.** The plan ends by saying ambiguous
> lines should get `SemanticRole::Unknown` **and** `confidence = 0.5`.
> `EntityRecord` has no `confidence` field and never got one — the only
> `confidence` values in the model are on `RelationEdge` (how much we trust an
> inferred edge) and `ClassificationRecord` (the LLM's self-report). Ambiguous
> entities are simply left `Unknown`, which carries the same information without
> inviting a made-up number to be averaged into something later.
>
> Note this classifier is Stage 7's rule-based role assignment. It is unrelated
> to `src/classification/`, the LLM classifier — the naming collision is
> unfortunate and worth remembering when grepping.

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

### Phase 3 — Relation Extractor (Weeks 4–5) — `[shipped]` `954b929`

> **Divergence — rules are not "first match wins".** The plan says the rules are
> "applied in order; first match wins". They are not: `extract` runs **every**
> rule and accumulates. An entity can legitimately be `PartOf` a trace *and*
> `RespondedTo` a prompt, and first-match would have silently dropped one of two
> true facts. Duplicate conclusions are made harmless instead by
> `make_edge`'s content-derived `relation_id` — two rules reaching the same
> `(type, source, target)` produce the same id and collapse on upsert.
>
> **Added later — Rule 9, `RespondedTo` via span parentage** (`cb3c202`,
> July 25). None of the eight planned rules fire on a pure MCP session: Rule 6
> needs an `AgentStep`, and a raw JSON-RPC log has none. So `mcp_session.log`
> produced 19 entities and 19 relations, all `PartOf` — the relation graph was 19
> disconnected nodes for the one input logflayer is built to ingest. The
> request/response pairing already existed as `parent_span_id` from Phase 1's
> third pass and simply was never turned into an edge. Rule 9 does that, at
> confidence 1.0 / `Explicit` since it comes from an id match rather than
> proximity.
>
> **Consequence of Rule 8 worth knowing about.** `PartOf`'s `target_entity_id`
> holds the 32-hex `otel_trace_id`, **not** an `entity_id` — the trace is not an
> entity record. Since every entity gets one, graph traversal excludes `PartOf`
> by default (`627f2fc`); otherwise every walk picks up the same unlabelled
> dead-end node and the graph view draws a mystery hub wired to everything. Pass
> `?include_structural=true` to opt in.

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

### Phase 4 — PROV Linker + OTel Builder (Week 5) — `[shipped]` `954b929`

> **Divergence — these are synchronous, not async.** The plan has them running
> "as the first async step after `Preprocessor::run()` stores `SampleMetadata`".
> They actually run **inside** `Preprocessor::run()` (`preprocessing/mod.rs`,
> `prov_linker::build` then `otel_builder::build`) and come back on
> `PipelineOutput` alongside the metadata. Both are pure CPU-bound transforms over
> data already in memory — making them async would have bought nothing and forced
> the caller to hold entities across an await. What *is* async is Phase 6: the
> writers that persist their output.
>
> **Divergence — field names are snake_case.** The plan's span JSON shows
> `traceId` / `spanId` / `startTimeUnixNano`. `OtelSpan` uses `trace_id`,
> `span_id`, `start_time_unix_nano`. Rust-default serde naming won; an OTLP
> exporter would do the camelCase conversion at the boundary, which is where it
> belongs.
>
> **Divergence — `wasAssociatedWith` was not implemented.** The plan lists it.
> `ProvPredicate` ships `WasGeneratedBy`, `WasAttributedTo`, `WasDerivedFrom`,
> `Used`, `ActedOnBehalfOf`. Attribution to a model agent is expressed with
> `wasAttributedTo` (entity → agent) rather than `wasAssociatedWith`
> (activity → agent).
>
> **Two things that shipped much later than this phase.** Both looked done from
> reading `otel_builder` alone, and both were silently broken:
>
> - **`start_time_unix_nano` was always 0** until `1a3079b` (July 25).
>   `otel_builder::timestamps` read `EntityRecord.timestamp_utc` correctly, but
>   Phase 1 hardcoded that field to `None` and nothing ever wrote it, so the
>   SpansView waterfall was ordered by entity index rather than time.
> - **`status.code` was always `UNSET`** until `f89b19f` (July 25) — `build`
>   passed `SpanStatus::default()`, so a failed tool call was indistinguishable
>   from a successful one. Now derived from the JSON-RPC error envelope, MCP's
>   `result.isError`, log severity, and completion outcome.
>
> **Policy call inside `status()`, easy to revisit.** `finish_reason = "length"`
> is **not** an error: truncation is routine for streaming and summarisation
> workloads, and OTel reserves `Error` for operations that failed, so counting it
> would inflate any error rate derived from span status. It is recorded as the
> `ug.finish.reason` attribute instead. `content_filter` *is* an error.

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

### Phase 5 — Embedding Worker (Weeks 6–8) — `[shipped]` `463039b`

> **Divergence — no change stream.** The plan has a tokio task tailing a change
> stream on `sample_metadata`. There is no change-stream code anywhere in `src/`.
> `EmbeddingWorker::embed_sample` is instead called directly from the pipeline's
> output stage with the entities already in hand. Change streams need a replica
> set — a standalone `mongod`, which is what the dev stack and most self-hosted
> deployments run, cannot serve them. Calling the worker inline also removes a
> whole class of "why is this sample not embedded yet" question.
>
> **Divergence — config is flat and env-driven, not a TOML table.** No
> `[embedding]` section: `EmbeddingConfig::from_env` reads `EMBEDDING_*` vars, so
> it matches how everything else in `AppConfig` is configured. `provider`,
> `batch_size` and `local_model_path` were never added — there is one HTTP path,
> pointed wherever `EMBEDDING_API_BASE_URL` says, which covers OpenAI plus any
> compatible endpoint (including a local one) without a provider enum. `api_key`
> falls back to `OPENAI_API_KEY`. `max_text_chars` replaced the token-count
> truncation, since counting characters needs no tokeniser.
>
> **Behavioral embeddings need no API key.** They are computed locally from
> structural features, so `EMBEDDING_ENABLED=false` still yields one behavioral
> vector per sample — which is what makes a vector-search endpoint testable
> without any credentials. Content embeddings are the part that needs the key.
>
> **Not yet exercised end to end:** content embeddings. Every smoke-test run to
> date has used `EMBEDDING_ENABLED=false`, so the OpenAI request path has no
> real-data verification behind it.

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

### Phase 6 — Output Adapters (Weeks 8–11) — `[shipped]` `94e5e08`, `9c978c8`

> **Divergence — no traits.** Both `GraphWriter` and `VectorWriter` are concrete
> structs, not the traits sketched below, and there is no `MongoGraphWriter` /
> `Neo4jGraphWriter` / `QdrantVectorWriter`. Async trait methods in stable Rust
> would have meant `#[async_trait]` and boxed futures for a second implementation
> nobody has written. The seam was kept where it costs nothing instead:
> `GRAPH_WRITER_BACKEND` / `VECTOR_WRITER_BACKEND` config values exist and
> default to `"mongodb"`, so adding a backend means adding a struct and a branch
> rather than reworking call sites. Note they are **not** validated: any string is
> accepted, and the `warn!` about an unknown value only fires when that writer is
> enabled — so `GRAPH_WRITER_BACKEND=neo4j` with the writer off is silently
> ignored.
>
> **Divergence — there is no `entity_nodes` collection.** The paragraph below
> promises one; it was never built. Entities live only in
> `sample_metadata.entities`, and `GraphWriter` writes `entity_edges`,
> `prov_relations` and `otel_spans` — there is no `write_entities` method. This
> matters for anyone querying: looking an entity up by id means a positional
> projection into the `entities` array of `sample_metadata` (see
> `MongoRepository::fetch_entity_by_id`), which is why there is a multikey index
> on `entities.entity_id`. A stale comment in `models.rs` claimed the
> `entity_nodes` collection existed and was removed in July 2026.
>
> **Divergence — `VectorWriter` has one method, `write`.** No
> `upsert_content_embedding` / `upsert_behavioral_embedding` split (both are
> `EmbeddingRecord`s, distinguished by a field, so one method suffices) and no
> `similarity_search`. **Nothing queries the embeddings.** They are written to
> `content_embeddings` / `behavioral_embeddings` and never read — that is the
> outstanding "vector search endpoint" work.
>
> **The Cypher example below is aspirational.** Multi-hop traversal shipped as
> `GET /api/v1/graph/{downstream,upstream,path}` over `entity_edges` in MongoDB
> (`ebb3a9c`, `25126aa`) — level-at-a-time BFS with one indexed `$in` query per
> depth, not Neo4j.

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

### Phase 7 — MCP Protocol Support (Weeks 9–10) — `[shipped]` `954b929`

> **Known gap — the parser's error fields are computed and discarded.**
> `McpParsed` carries `is_error`, `error_code` and `error_message`, extracted at
> higher fidelity than anything downstream can reconstruct. The only caller,
> `entity_extractor`, takes `tool_name` and `server_id` and drops the rest,
> because `EntityRecord` has no error field to hold them. So
> `otel_builder::status` re-derives error state from raw `extracted_fields`
> instead — and the two disagree in a small way: `extract_error` requires `error`
> to be an object, `status()` also accepts a bare string.
>
> Closing this means adding error fields to `EntityRecord` and threading them
> through, after which `status()` can prefer the parsed form for `McpEvent`
> entities. Worth doing; not done.

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

## 6. Configuration Additions — `[shipped]` `14ee734`

**As actually implemented.** Every one of these is read by
`AppConfig::from_env` and listed in `.env.example`; the full set is verified
against `src/config.rs`:

```
ENTITY_EXTRACTION_ENABLED=true         # master switch for stages 6–10
ENTITY_EXTRACTION_MIN_ENTITIES=1       # skip async output writes below this count

GRAPH_WRITER_ENABLED=false             # entity_edges / prov_relations / otel_spans
GRAPH_WRITER_BACKEND=mongodb           # only "mongodb" is wired; others warn and fall back

VECTOR_WRITER_ENABLED=false            # content_embeddings / behavioral_embeddings
VECTOR_WRITER_BACKEND=mongodb          # only "mongodb" is wired

EMBEDDING_ENABLED=false                # CONTENT embeddings only — see note below
EMBEDDING_API_KEY=                     # falls back to OPENAI_API_KEY
EMBEDDING_API_BASE_URL=                # defaults to https://api.openai.com
EMBEDDING_MODEL=text-embedding-3-small
EMBEDDING_MAX_TEXT_CHARS=8000          # ~2 000 tokens; replaced the planned token count
EMBEDDING_DIMENSIONS=1536              # 0 = model's native dimensionality
```

**Not implemented:** `EMBEDDING_PROVIDER`, `NEO4J_URI`, `NEO4J_USERNAME`,
`NEO4J_PASSWORD`, `QDRANT_URL`, `QDRANT_API_KEY`. The backends they configure
do not exist (see Phase 6), and `EMBEDDING_API_BASE_URL` covers redirecting to a
different provider without a provider enum.

**Two traps in the naming:**

- `EMBEDDING_ENABLED=false` does **not** mean "no embeddings". It disables
  *content* embeddings, the ones needing an API call. Behavioral embeddings are
  computed locally, so a run with `EMBEDDING_ENABLED=false` still produces one
  behavioral vector per sample — provided `VECTOR_WRITER_ENABLED=true` *and* the
  sample clears `ENTITY_EXTRACTION_MIN_ENTITIES`. With the default of `1`, a run
  with `ENTITY_EXTRACTION_ENABLED=false` writes no embeddings at all.
- `ENTITY_EXTRACTION_MIN_ENTITIES` gates the **output writes**, not the
  extraction. Entities and relations still land in `sample_metadata`; what gets
  skipped below the threshold is the graph/vector persistence. Setting it to `0`
  disables the gate entirely — including for samples with *no* entities, which
  still get a behavioral embedding written (the lineage write early-returns on
  empty, but the embedding write does not).

---

## 7. Backfill — `[shipped]`, with one deliberate limitation

> **Decision — backfill rebuilds `sample_metadata` only; it does not write graph
> or vector output.** `backfill.rs` calls `prep.run(...).metadata` and discards
> `prov_triples` / `otel_spans`. The output adapters are wired into the live
> sampling path only.
>
> The reasoning is in the code comment: re-running graph and vector writes during
> a backfill risks duplicating work, and is not needed for the thing backfill
> exists to fix — a missing or stale `sample_metadata` document.
>
> **The practical consequence, which is easy to trip over:** backfilling a v1
> sample gives it entities and relations *in its metadata document* but leaves
> `entity_edges`, `prov_relations`, `otel_spans` and the embedding collections
> untouched. So the Relations / PROV / Spans views will show data for that sample
> (they fall back to client-side synthesis from `metadata`) while the traversal
> endpoints, which read `entity_edges` directly, will find nothing. If you need
> the auxiliary collections populated for historical samples, that is currently a
> gap — re-ingesting is the only route.
>
> Note also that the writes *are* idempotent (content-derived ids, upsert
> filters), so the duplication concern is milder than it was when this was
> decided. Enabling output writes during backfill would now be safe; it simply
> has not been done.

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

> **Historical.** All nine rows landed. The real sequence was compressed:
> Phases 0–7 all went in on July 17 2026 as 13 commits, `14ee734`…`82e3955`
> (`git rev-list 14ee734^..82e3955`). The UI (`95aa3c3`, `82e3955`) and the plan
> documents themselves (`248a229`) are *inside* that range, not after it, and
> `backfill.rs` predates the whole batch — it arrived with the initial commit in
> April. The week numbers below never corresponded to anything.

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

---

## 11. Decisions made during implementation

Everything above §10 was written before the work started. These were decided
while building it, and are the reasoning most likely to be needed later.

**The frontend keeps client-side synthesis as a fallback** — for PROV and spans.
`provTriples` and `otelSpans` prefer the server-fetched collection and fall back
to deriving from `metadata.entities` + `metadata.relations` when it is empty. That
keeps those views useful with `GRAPH_WRITER_ENABLED=false` and for backfilled
samples whose auxiliary collections were never written (§7). `serverDataAvailable`
tells a view which mode it is in.

The cost: synthesised spans carry zero timestamps and `UNSET` status regardless of
what the log said, and the relation-type → PROV-predicate mapping is crude. Treat
any disagreement between the two as the client fallback being wrong.

> **Inconsistency worth fixing:** `relations` does **not** follow this pattern.
> It is `selected.value?.relations ?? []` — always the metadata copy, never the
> server's. `serverRelations` is fetched and stored but no view reads it; its only
> effect is contributing to the `serverDataAvailable` OR. So the Relations view and
> the graph are always on synthesised data even when `entity_edges` is populated.
> Harmless today because the two agree in content, but it means the view silently
> ignores anything the graph writer added.

**Vocabulary alignment is one-way: the backend is the source of truth.** The
frontend types in `types/index.ts` are meant to mirror the backend's canonical
serialisation exactly — `RESPONDED_TO` / `GENERATED` / `INFORMED` /
`DELEGATED_TO`, snake_case `ClassificationStatus`, real `EntityRecord` field
names. When they drift, the frontend changes.

> **This drifted, and it drifted silently.** `EntityType` was declared PascalCase
> (`'PromptEvent'`) against a backend carrying
> `#[serde(rename_all = "snake_case")]` that puts `prompt_event` on the wire; and
> `RelationSource` omitted `'inferred'`, which is the `#[default]` and the source
> of most relation rules. Nothing crashed. The Entities type filter matched
> nothing, every type badge fell through to the default colour, and
> `entityTypeToSpanKind` returned `INTERNAL` for every entity. Fixed July 25 2026.
>
> The lesson: a string-union mismatch fails by matching nothing rather than by
> throwing, so `vue-tsc` passing says nothing about whether the literals are
> right. Check them against the serde attributes in `models.rs` — or against real
> API output — not against the Rust variant names, which is exactly the mistake
> that was made here.

**PROV views strip URI prefixes at the display layer.** `ProvView.entityLabel`
strips `ug:(entity|activity|agent):` before resolving a label, because
server-side triples carry URIs while client-synthesised ones carry bare ids and
both must render. `GET /api/v1/entities/:id` accepts either spelling for the same
reason. Cross-sample references resolve through that endpoint and cache negative
results, so a genuinely missing entity is not re-requested on every render.

**Graph traversal excludes structural edges by default** (`627f2fc`). See
Phase 3 — `PartOf` targets the trace, not an entity, and every entity has one.

**Span status treats truncation as success** (`f89b19f`). See Phase 4 —
`finish_reason = "length"` is recorded as an attribute rather than counted as an
error.

---

## 12. Known gaps

Ordered roughly by how much they cost:

1. **Nothing queries the embeddings.** `content_embeddings` and
   `behavioral_embeddings` are written and never read. A `POST /api/v1/search`
   over them is the outstanding piece of Phase 6B. Behavioral vectors need no API
   key, so this is testable without credentials.
2. **Content embeddings have never been exercised end to end.** Every smoke-test
   run has used `EMBEDDING_ENABLED=false`, so the OpenAI request path has no
   real-data verification behind it.
3. **Backfill does not populate the auxiliary collections** (§7).
4. **MCP error detail is discarded** (Phase 7).
5. **No `entity_nodes` collection**, despite §6A promising one (Phase 6). Entity
   lookup goes through a positional projection into `sample_metadata` instead,
   which works but ties entity queries to the parent document.
