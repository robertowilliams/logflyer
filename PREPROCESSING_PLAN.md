# Logflayer Preprocessing Pipeline — Implementation Plan

**Author:** Roberto Williams Batista  
**Date:** 2026-04-26  
**Status:** ✅ **Shipped** — Phases 1, 2 and 3 all landed. This is now a record,
not a forward plan.  
**Scope:** `logflayer/` (producer) · `vectadb-agents/logflayersense/` (consumer)
— both exist; see the note below

> **Read this first: classification exists twice.**
>
> Phase 2 shipped as planned, in the separate crate the Scope line names:
> `code/vectadb_engine/vectadb-agents/logflayersense/` (`vectadb-logflayersense`).
> All four planned files are there and implemented — `mongo_reader.rs`,
> `prompt_builder.rs`, `classifier.rs`, `main.rs` with a `run_mongo_loop` polling
> loop. **Note it is untracked in git** (`git status` in `vectadb_engine` shows
> `?? vectadb-agents/logflayersense/`), so it will not appear in a `git grep` and
> is not backed up by the repo. Worth committing.
>
> But `logflayer` **also** grew an in-process classifier at `src/classification/`
> (`client.rs`, `prompt.rs`), triggered inline from `service/application.rs`
> rather than by polling. So there are now two implementations of Phase 2:
>
> | Phase 2 step | `logflayersense` (planned, exists) | `logflayer` (also exists) |
> | --- | --- | --- |
> | 2.1 read from Mongo | `src/mongo_reader.rs` — polls `sample_metadata` | `repository/mongo.rs::fetch_pending_classifications` — **defined but called only by tests** |
> | 2.2 build prompt | `src/prompt_builder.rs` | `classification/prompt.rs` |
> | 2.3 classify | `src/classifier.rs` | `classification/client.rs` |
> | 2.4 drive it | `src/main.rs` poll loop | inline in `service/application.rs` |
>
> **This overlap is unresolved and is the thing to sort out.** Which one is
> authoritative determines whether `fetch_pending_classifications` is dead code or
> the real entry point, and whether `CLASSIFICATION_ENABLED` in `logflayer` should
> default off. It also bears directly on the standalone-vs-monorepo question in
> `docs/next_steps_may_1st.md` item 10.
>
> **Last verified against the code:** July 25, 2026.

---

## 1. Context and Objective

`logflayer` currently samples raw log text from remote hosts via SSH and stores it verbatim in MongoDB. `logflayersense` then hands that raw text to an LLM and asks it to identify agentic events — without any knowledge of format, structure, or signal density.

The goal of this plan is to insert a **preprocessing stage** inside `logflayer` that runs immediately after each sample is stored. The preprocessor analyses the raw content and writes a `SampleMetadata` document to MongoDB. `logflayersense` is then updated to read both the raw sample and its metadata before calling the LLM, enabling context-aware classification and eliminating wasted LLM calls on non-agentic logs.

```
BEFORE:
  logflayer → raw sample → MongoDB → logflayersense → LLM (blind)

AFTER:
  logflayer → raw sample → MongoDB
            → preprocessing pipeline
            → SampleMetadata → MongoDB
  logflayersense → raw sample + metadata → informed LLM call → VectaDB
```

---

## 2. Architecture Overview

### 2.1 New module in `logflayer`

```
logflayer/src/
├── preprocessing/
│   ├── mod.rs              # Pipeline orchestrator
│   ├── format_detector.rs  # Log format detection
│   ├── stats.rs            # Statistical profile
│   ├── agentic_scanner.rs  # Regex-based agentic signal scan
│   ├── schema_extractor.rs # Structured log schema inference
│   └── hints.rs            # Ingestion hint generator
├── models.rs               # + SampleMetadata, PreprocessingResult
└── repository/
    └── mongo.rs            # + store_metadata(), fetch_pending_samples()
```

### 2.2 Updated module in `logflayersense`

```
logflayersense/src/
├── mongo_reader.rs         # NEW — reads SampleRecord + SampleMetadata
├── prompt_builder.rs       # NEW — builds LLM prompt from metadata hints
├── classifier.rs           # UPDATED — accepts metadata context
└── main.rs                 # UPDATED — drives from MongoDB instead of local files
```

---

## 3. Data Contracts

### 3.1 `SampleRecord` (existing — no breaking changes)

```rust
pub struct SampleRecord {
    pub timestamp: DateTime,
    pub sample_hash: String,       // SHA-256, primary key
    pub target_id: String,
    pub source_file: String,
    pub sample_content: String,
    pub host: String,
    pub path: String,
    pub sampling_mode: SamplingMode,
    pub line_count: Option<u64>,
    pub file_size_bytes: Option<u64>,
    pub processing_status: ProcessingStatus,
    pub error_details: Option<String>,
}
```

### 3.2 `SampleMetadata` (new document)

Stored in a new MongoDB collection `sample_metadata`, linked to `SampleRecord` via `sample_hash`.

```rust
pub struct SampleMetadata {
    pub sample_hash: String,           // FK → SampleRecord
    pub target_id: String,
    pub analyzed_at: DateTime,
    pub preprocessing_version: String, // semver — invalidates stale metadata on upgrade

    pub format: LogFormat,
    pub stats: SampleStats,
    pub agentic_scan: AgenticScan,
    pub schema: Option<LogSchema>,     // None for unstructured logs
    pub ingestion_hints: IngestionHints,
}

pub struct LogFormat {
    pub log_type: LogType,             // Json | Logfmt | Syslog | PlainText | Mixed
    pub timestamp_field: Option<String>,
    pub level_field: Option<String>,
    pub message_field: Option<String>,
    pub timestamp_format: Option<String>, // e.g. RFC3339, Unix, custom strftime
    pub multiline: bool,               // Python tracebacks, Java stack traces
}

pub struct SampleStats {
    pub total_lines: u64,
    pub non_empty_lines: u64,
    pub empty_line_ratio: f32,
    pub avg_line_length: f32,
    pub time_span_secs: Option<u64>,   // delta between first/last timestamp
    pub earliest_timestamp: Option<DateTime>,
    pub latest_timestamp: Option<DateTime>,
    pub level_distribution: HashMap<String, u64>, // {"INFO": 150, "ERROR": 30}
    pub unique_line_ratio: f32,        // detects repetitive/noisy logs
}

pub struct AgenticScan {
    pub signal_score: f32,             // 0.0–1.0
    pub worth_classifying: bool,       // signal_score >= configured threshold
    pub detected_frameworks: Vec<String>, // ["langchain", "crewai", "bedrock"]
    pub matched_patterns: Vec<String>, // ["tool_call", "llm_response", "chain_step"]
    pub agentic_line_count: u64,       // lines that matched at least one pattern
}

pub struct LogSchema {
    pub fields: Vec<FieldInfo>,
    pub sample_coverage: f32,         // fraction of lines this schema covers
}

pub struct FieldInfo {
    pub name: String,
    pub inferred_type: FieldType,     // String | Number | Bool | Object | Array
    pub presence_ratio: f32,          // fraction of lines containing this field
    pub is_identifier: bool,          // high cardinality — agent_id, trace_id, etc.
}

pub struct IngestionHints {
    pub prompt_template: PromptTemplate, // JsonAgent | PlainAgent | Syslog | Generic
    pub suggested_chunk_size: usize,   // lines per LLM call
    pub skip_reason: Option<String>,   // set when worth_classifying == false
    pub priority: u8,                  // 0–255, higher = process first
}
```

### 3.3 MongoDB collections

| Collection | Key | Description |
|---|---|---|
| `log_samples` | `sample_hash` | Raw sample content (existing) |
| `sample_metadata` | `sample_hash` | Preprocessing output (new) |
| `targets` | `_id` | SSH targets (existing) |

Index required on `sample_metadata`:
- Unique: `sample_hash`
- Compound: `(target_id, analyzed_at)` — for logflayersense polling
- Single: `ingestion_hints.worth_classifying` — for fast filtering

---

## 4. Implementation Plan

### Phase 1 — Preprocessing Pipeline in `logflayer` — `[shipped]`

> Stages 1–5 landed as described, in `src/preprocessing/`. Two notes:
>
> - **The pipeline now has ten stages, not five.** Stages 6–10 (entity
>   extraction, semantic roles, relations, PROV, OTel) were added by
>   `UPSIDEGATE_PLAN.md` and live in the same module. `Preprocessor::run` returns
>   a `PipelineOutput` — `SampleMetadata` **plus** `prov_triples` and
>   `otel_spans` — not a bare `SampleMetadata` as the steps below imply. Any code
>   written against the older signature will not compile; that is exactly what
>   broke `tests/integration_test.rs` for several weeks (fixed in `6f81a13`).
> - **`stats::extract_level_plain` is now `pub(crate)`** and shared with
>   `otel_builder`, so severity classification for unstructured lines lives in one
>   place instead of being reimplemented per consumer.

#### Step 1.1 — Add `SampleMetadata` to `models.rs`

Define all structs from section 3.2. Derive `Serialize`, `Deserialize` for BSON/MongoDB. Add `preprocessing_version` constant (`"1.0.0"`) so stale metadata can be detected and reprocessed after upgrades.

#### Step 1.2 — `format_detector.rs`

```
Input:  &str (raw sample content)
Output: LogFormat
```

Detection logic (in order of priority):

1. **JSON** — try `serde_json::from_str` on first non-empty line. If succeeds, confirm >60% of lines parse as JSON objects.
2. **Logfmt** — look for `key=value` or `key="value"` patterns across >50% of lines.
3. **Syslog (RFC 5424 / 3164)** — match `<priority>timestamp hostname app:` prefix regex.
4. **Multiline** — detect Java stack traces (`\tat `), Python tracebacks (`Traceback`, `File "`, `  File `).
5. **Plain text** — fallback.

For each structured format, extract field names for timestamp, level, and message using a lookup table of common conventions:
- Timestamp: `time`, `timestamp`, `ts`, `@timestamp`, `datetime`
- Level: `level`, `severity`, `lvl`, `log_level`, `loglevel`
- Message: `msg`, `message`, `text`, `body`, `log`

#### Step 1.3 — `stats.rs`

```
Input:  &str (raw content), &LogFormat
Output: SampleStats
```

Single-pass over lines:
- Count total, non-empty, unique lines
- Extract timestamps using detected format → compute `time_span_secs`
- Detect log level per line (regex against common values: DEBUG, INFO, WARN, WARNING, ERROR, CRITICAL, FATAL) → build `level_distribution`
- Compute `avg_line_length` and `unique_line_ratio` (unique lines / total)

#### Step 1.4 — `agentic_scanner.rs`

```
Input:  &str (raw content)
Output: AgenticScan
```

Compile a static `once_cell::sync::Lazy<Vec<(PatternName, Regex)>>` pattern registry at startup. Pattern groups:

| Pattern name | Example matches |
|---|---|
| `tool_call` | `tool_name:`, `"tool":`, `Action:`, `> Invoking:` |
| `llm_response` | `Thought:`, `Final Answer:`, `AI:`, `assistant:` |
| `chain_step` | `> Entering new`, `> Finished chain`, `Running step` |
| `agent_handoff` | `Transferring to`, `Delegating to`, `handoff` |
| `embedding_op` | `embed(`, `get_embedding`, `similarity_search` |
| `model_invoke` | `invoke_model`, `InvokeModel`, `chat.completions` |

Framework detection (keyword scan on entire sample):
- `langchain` — `langchain`, `LangChain`, `from langchain`
- `crewai` — `crewai`, `CrewAI`, `crew.kickoff`
- `autogen` — `autogen`, `AutoGen`, `GroupChat`
- `bedrock` — `bedrock`, `invoke_model`, `anthropic.claude`
- `openai` — `openai`, `chat/completions`, `gpt-`
- `llamaindex` — `llama_index`, `LlamaIndex`, `QueryEngine`

`signal_score` = `agentic_line_count / total_lines` (capped at 1.0).  
`worth_classifying` = `signal_score >= config.agentic_threshold` (default `0.02` — 2% of lines must match).

#### Step 1.5 — `schema_extractor.rs`

```
Input:  &str (raw content), &LogFormat
Output: Option<LogSchema>
```

Only runs when `log_type == Json` or `log_type == Logfmt`.

For JSON logs:
- Parse up to 200 lines as `serde_json::Value::Object`
- Collect all keys seen across parsed lines
- For each key: infer type from majority value type, compute `presence_ratio`
- Flag as `is_identifier` if string field has >90% unique values across sample

For Logfmt:
- Extract all `key=` occurrences, same frequency analysis

Skip schema extraction for plain text and syslog — not worth the complexity.

#### Step 1.6 — `hints.rs`

```
Input:  &LogFormat, &SampleStats, &AgenticScan
Output: IngestionHints
```

`prompt_template` selection:
- `LogType::Json` + `worth_classifying` → `JsonAgent`
- `LogType::Logfmt` + `worth_classifying` → `LogfmtAgent`
- `LogType::Syslog` → `Syslog`
- anything else → `Generic`

`suggested_chunk_size`:
- Long lines (`avg_line_length > 300`) → 10 lines/chunk
- Short lines (`avg_line_length < 80`) → 50 lines/chunk
- Default → 20 lines/chunk

`priority`:
- High `signal_score` + `ERROR`/`FATAL` in level distribution → 200+
- `worth_classifying` → 100
- otherwise → 10

`skip_reason` examples:
- `"signal_score below threshold (0.00)"`
- `"sample is empty"`
- `"all lines are duplicates (ratio=1.0)"`

#### Step 1.7 — `preprocessing/mod.rs` — Pipeline orchestrator

```rust
pub struct Preprocessor {
    config: PreprocessingConfig,
}

impl Preprocessor {
    pub fn run(&self, content: &str) -> PreprocessingResult {
        let format   = format_detector::detect(content);
        let stats    = stats::compute(content, &format);
        let scan     = agentic_scanner::scan(content, &self.config);
        let schema   = schema_extractor::extract(content, &format);
        let hints    = hints::generate(&format, &stats, &scan, &self.config);

        PreprocessingResult { format, stats, scan, schema, hints }
    }
}
```

The preprocessor is synchronous and CPU-bound. Wrap the call in `tokio::task::spawn_blocking` in the async context of `application.rs`.

#### Step 1.8 — `repository/mongo.rs` — New methods

```rust
// Store metadata document; upsert by sample_hash
pub async fn store_metadata(&self, metadata: &SampleMetadata) -> Result<(), AppError>;

// Fetch samples that have no metadata yet (backfill support)
pub async fn fetch_unprocessed_samples(&self, limit: usize) -> Result<Vec<SampleRecord>, AppError>;
```

#### Step 1.9 — Wire into `service/application.rs`

In `process_document`, after `repository.store_sample()` returns `StoreOutcome::Inserted`:

```rust
// Run preprocessing on newly stored sample
let preprocessor = Preprocessor::new(config.preprocessing.clone());
let result = tokio::task::spawn_blocking(move || {
    preprocessor.run(&sample.sample_content)
}).await?;

let metadata = SampleMetadata::from_result(&sample, result);
repository.store_metadata(&metadata).await?;
```

Do not preprocess on `StoreOutcome::Duplicate` — metadata already exists.

#### Step 1.10 — Configuration additions — `[shipped]`

> **Divergence — env vars, not a TOML table.** No `[preprocessing]` section
> exists; `PreprocessingConfig` is built by `AppConfig::from_env`, matching how
> the rest of the config works. The three planned knobs shipped with a
> `PREPROCESSING_` prefix, and three more were added later.

**As actually implemented** (`src/config.rs`, all present in `.env.example`):

```
PREPROCESSING_ENABLED=true                       # kill switch for the whole pipeline
PREPROCESSING_AGENTIC_THRESHOLD=0.02             # min signal_score to mark worth_classifying
PREPROCESSING_MAX_SCHEMA_LINES=200               # lines sampled for schema extraction
PREPROCESSING_REPROCESS_ON_VERSION_CHANGE=false  # see Step 3.2
METRICS_PORT=9090                                # note: no PREPROCESSING_ prefix

# Added by UpsideGate (see UPSIDEGATE_PLAN.md §6) — same struct, stages 6–10:
ENTITY_EXTRACTION_ENABLED=true
ENTITY_EXTRACTION_MIN_ENTITIES=1
```

**The two switches are nested, not independent.** `ENTITY_EXTRACTION_ENABLED`
gates stages 6–10 *within* a pipeline run, so turning it off still produces a
`SampleMetadata` document — just one with empty `entities` and `relations`. But
`PREPROCESSING_ENABLED=false` skips the run entirely
(`service/application.rs`), so **no metadata document is written at all** and
stages 6–10 never execute regardless of the second flag.

---

### Phase 2 — Update `logflayersense` to consume metadata — `[shipped]`

> Shipped as written, in `vectadb-agents/logflayersense/` — all four steps below
> exist in that crate. But `logflayer` grew a parallel in-process classifier at
> `src/classification/` as well. See the note at the top of this document: the
> duplication is real and unresolved.

#### Step 2.1 — `mongo_reader.rs` (new)

Replace the current local file tailing approach with a MongoDB reader that polls for unclassified, `worth_classifying == true` samples, ordered by `priority DESC, analyzed_at ASC`.

```rust
pub struct MongoReader {
    db: MongoRepository,
    poll_interval: Duration,
}

impl MongoReader {
    pub async fn poll(&self) -> Result<Vec<(SampleRecord, SampleMetadata)>, AppError>;
    pub async fn mark_classified(&self, sample_hash: &str) -> Result<(), AppError>;
}
```

Add a `classification_status` field to `SampleMetadata`:

```rust
pub enum ClassificationStatus {
    Pending,
    Classified,
    Skipped,
    Failed { reason: String },
}
```

#### Step 2.2 — `prompt_builder.rs` (new)

Build the LLM prompt using metadata rather than raw text alone.

```rust
pub fn build_prompt(
    content: &str,
    metadata: &SampleMetadata,
) -> String
```

Template selection from `ingestion_hints.prompt_template`:

- **JsonAgent** — tells the LLM the field names for timestamp, level, message; which fields likely contain agent identity; what frameworks were detected.
- **LogfmtAgent** — similar, adapted for `key=value` format.
- **Generic** — minimal prompt, raw text only.

Example preamble for `JsonAgent`:

```
This log sample is in JSON format. Each line is a JSON object.
Timestamp field: "time" | Level field: "severity" | Message field: "msg"
Detected frameworks: LangChain, OpenAI
Fields of interest: agent_id, tool_name, session_id
Time range: 2026-04-26 10:00 → 10:58 (3,412 lines, signal score: 0.31)

Classify each line below as agentic (true/false) and extract event_type if agentic.
```

#### Step 2.3 — `classifier.rs` (update)

Accept `Option<&SampleMetadata>` and pass to `prompt_builder`. Fall back to current blind prompt if metadata is absent (backward compatibility).

Respect `ingestion_hints.suggested_chunk_size` for batching instead of the fixed global config value.

#### Step 2.4 — `main.rs` (update)

Replace the local file watch loop with a MongoDB polling loop using `MongoReader`. The config gains a `mongo` section matching logflayer's connection config. The `log_files` config section is removed.

---

### Phase 3 — Backfill and Observability — `[shipped]`, with one item dropped

#### Step 3.1 — Backfill command — `[shipped]`

> `logflayer backfill --batch-size N --dry-run` works as described
> (`src/main.rs::run_backfill`, `src/backfill.rs`).
>
> **Limitation worth knowing:** backfill rebuilds `SampleMetadata` only. It does
> not write `entity_edges`, `prov_relations`, `otel_spans` or the embedding
> collections — the output adapters are wired into the live sampling path only.
> See `UPSIDEGATE_PLAN.md` §7 for the reasoning and the practical consequence.

Add a CLI subcommand `logflayer backfill` that runs the preprocessing pipeline over all `SampleRecord` documents that have no corresponding `SampleMetadata`. Processes in batches of 100, respects concurrency limit. Useful for existing data and after preprocessing version upgrades.

```
logflayer backfill --batch-size 100 --dry-run
```

#### Step 3.2 — Version-aware reprocessing — `[shipped]`

> The opt-in flag exists as planned, spelled
> `PREPROCESSING_REPROCESS_ON_VERSION_CHANGE`, defaulting to `false`. `main.rs`
> gates the startup call on that flag **and** `preprocessing.enabled`, so nothing
> happens on an ordinary upgrade.
>
> **Divergence — stale metadata is deleted, not queued.** When the flag is on,
> `backfill::purge_stale_metadata` runs a `delete_many` over documents whose
> `preprocessing_version` differs from the binary's, rather than adding them to a
> reprocessing queue. The `SampleRecord`s are untouched, so the next backfill or
> sampling cycle regenerates the metadata from scratch. Deleting cannot leave
> half-migrated documents behind, which a queue can.
>
> There is a second entry point the plan did not anticipate:
> `logflayer backfill --reprocess_stale`.
>
> ⚠️ `PREPROCESSING_REPROCESS_ON_VERSION_CHANGE` is **missing from
> `.env.example`** — the only env var read in `src/` that is not documented there,
> aside from the `OPENAI_API_KEY` fallback.

#### Step 3.3 — Metrics — `[shipped]`, renamed

> Every planned metric exists in some form, but none kept its planned name.
> **Use these:**

| Planned | Actual (`src/metrics.rs`) |
| ------- | ------------------------- |
| `logflayer_preprocessed_total` | `logflayer_preprocessing_samples_processed_total` |
| `logflayer_agentic_detected_total` | `logflayer_preprocessing_agentic_signals_total` |
| `logflayer_preprocessing_duration_ms` | `logflayer_preprocessing_duration_seconds` (seconds, per Prometheus convention) |
| `logflayer_format_detected{type}` | `logflayer_format_detected` — unchanged |
| `logflayer_schema_extracted_total` | `logflayer_schema_extracted_total` — unchanged |

Two more were added that the plan did not anticipate:
`logflayer_preprocessing_samples_skipped_total` and
`logflayer_preprocessing_errors_total`.

---

## 5. File Checklist

### `logflayer/`

| File | Action |
|---|---|
| `src/models.rs` | Add `SampleMetadata`, `LogFormat`, `SampleStats`, `AgenticScan`, `LogSchema`, `FieldInfo`, `IngestionHints`, `PromptTemplate`, `FieldType`, `LogType` |
| `src/preprocessing/mod.rs` | New — pipeline orchestrator |
| `src/preprocessing/format_detector.rs` | New |
| `src/preprocessing/stats.rs` | New |
| `src/preprocessing/agentic_scanner.rs` | New |
| `src/preprocessing/schema_extractor.rs` | New |
| `src/preprocessing/hints.rs` | New |
| `src/repository/mongo.rs` | Add `store_metadata()`, `fetch_unprocessed_samples()` |
| `src/config.rs` | Add `PreprocessingConfig` |
| `src/service/application.rs` | Wire preprocessor after `store_sample()` |
| `src/lib.rs` | Export `preprocessing` module |
| `Cargo.toml` | Add `once_cell`, `regex` dependencies |

### `logflayersense/` — `[shipped]`, at `code/vectadb_engine/vectadb-agents/logflayersense/`

Every row landed, including the `models.rs` decision: the crate **duplicates**
the types rather than importing them, so `SampleMetadata` and friends are declared
in both crates and must be kept in step by hand. `Cargo.toml` carries the
`mongodb = "2.8"` the plan called for.

`main.rs` kept the legacy `FileTailer` alongside the new `run_mongo_loop`, so the
file-tailing path was added to rather than replaced.

> ⚠️ **The crate is untracked in git.** Nothing in `vectadb_engine`'s history
> contains it. Committing it should come before any further work on it.

---

## 6. Dependencies to Add — `[shipped]`

### `logflayer/Cargo.toml`

```toml
once_cell = "1.19"
regex = "1.10"
```

`regex` may already be present — confirm before adding.

> Both present. `chrono` also became load-bearing later, for timestamp parsing in
> `entity_extractor` (`1a3079b`).

### `logflayersense/Cargo.toml`

```toml
mongodb = "2.8"
```

> Present as specified. The crate also pulls in `uuid` v4, which `logflayer`
> deliberately does not (see `UPSIDEGATE_PLAN.md` Phase 0 on derived ids).

---

## 7. Testing Plan

### Unit tests (in `logflayer`)

| Test | Location | What it covers |
|---|---|---|
| `test_detect_json_log` | `format_detector.rs` | JSON format correctly identified |
| `test_detect_logfmt` | `format_detector.rs` | Logfmt correctly identified |
| `test_detect_plaintext` | `format_detector.rs` | Fallback to plain text |
| `test_stats_time_span` | `stats.rs` | Correct time delta from timestamps |
| `test_stats_level_distribution` | `stats.rs` | Level counts match fixture |
| `test_agentic_scan_langchain` | `agentic_scanner.rs` | LangChain patterns detected |
| `test_agentic_scan_no_signal` | `agentic_scanner.rs` | Nginx log scores 0.0 |
| `test_schema_json_fields` | `schema_extractor.rs` | Fields extracted with correct types |
| `test_hints_chunk_size` | `hints.rs` | Chunk size varies with line length |
| `test_pipeline_end_to_end` | `preprocessing/mod.rs` | Full run on fixture log file |

Fixture log files live in `logflayer/tests/fixtures/`:
- `langchain_json.log` — JSON LangChain trace
- `nginx_access.log` — plain HTTP access log (non-agentic)
- `bedrock_multiline.log` — AWS Bedrock with tracebacks
- `crewai_logfmt.log` — CrewAI in logfmt format

### Integration tests

- `logflayer` stores `SampleRecord` + `SampleMetadata` in a real MongoDB test instance (use `testcontainers` crate)
- `logflayersense` reads both documents, builds prompt correctly, verifies skip logic on `worth_classifying = false`

---

## 8. Milestones

| # | Milestone | Deliverable |
|---|---|---|
| M1 | Models + MongoDB schema | `SampleMetadata` struct, collection, indexes |
| M2 | Format detector | `format_detector.rs` + unit tests + fixtures |
| M3 | Stats + agentic scanner | `stats.rs`, `agentic_scanner.rs` + unit tests |
| M4 | Schema extractor + hints | `schema_extractor.rs`, `hints.rs` + unit tests |
| M5 | Pipeline wired in logflayer | `preprocessing/mod.rs`, service integration |
| M6 | logflayersense reads MongoDB | `mongo_reader.rs`, config update |
| M7 | Context-aware prompts | `prompt_builder.rs`, classifier update |
| M8 | Backfill CLI + metrics | Observability and operational tooling |
| M9 | Integration tests | End-to-end test suite |

Each milestone is independently shippable — M1–M5 can be deployed without touching logflayersense at all.

> All nine landed. M6 and M7 shipped as `src/classification/` inside this crate
> rather than in a second service.

---

## 9. What changed after this plan

This document covers stages 1–5 of what is now a ten-stage pipeline. For
everything after it:

- **`UPSIDEGATE_PLAN.md`** — stages 6–10 (entity extraction, semantic roles,
  relations, PROV, OTel), the graph and vector output adapters, MCP parsing, and
  the config flags that gate all of it. Also carries the decision log and the
  current list of known gaps.
- **`SMOKETEST.md`** — how to verify the whole pipeline end to end against a live
  MongoDB, with real per-fixture figures.

**One operational lesson worth carrying forward.** Three defects in the
UpsideGate work were invisible to `cargo test --lib` and only showed up when the
smoke test ran against real data: a field that was never populated, a fixture
that produced no relations, and edges pointing at a non-entity. All three
produced plausible-looking output with a whole dimension missing. Unit tests
cannot catch a field that is uniformly absent, because the assertions get written
against the same wrong assumption as the code.

So: use `cargo test --all-targets` rather than `--lib` (the integration tests
went uncompiled for weeks behind a green `--lib`), and re-run `SMOKETEST.md`
after pipeline changes rather than trusting green unit tests.
