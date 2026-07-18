# UpsideGate End-to-End Smoke Test

Verifies the full preprocessing + async-output pipeline (entity extraction →
graph writer → vector writer → API → UI) without needing an SSH-reachable
target. Run this after any change to the pipeline wiring or the new
`/api/v1/{relations,prov,spans}` endpoints.

## Prerequisites

- A running MongoDB reachable via `MONGODB_URI`.
- Rust toolchain (`cargo`).
- Node 20+ + npm if you want to verify the UI side.

```bash
# Quick local Mongo (skip if you already have one):
docker run -d -p 27017:27017 --name mongo mongo:7
```

## 1. Configure environment

The smoke-test subcommand honours all the same env vars as the live service.
The minimum useful set:

```bash
export MONGODB_URI=mongodb://localhost:27017
export ENTITY_EXTRACTION_ENABLED=true
export GRAPH_WRITER_ENABLED=true
export VECTOR_WRITER_ENABLED=true

# Optional — content embeddings need an API key.
# Behavioral embeddings work without it.
export EMBEDDING_ENABLED=false
```

Any of these can also be set via `.env` in the `logflayer/` directory; see
`.env.example` for the full list.

## 2. Run the smoke test

```bash
cd logflayer
cargo run -- smoketest tests/fixtures/mcp_session.log
```

Other fixtures worth trying:

```bash
cargo run -- smoketest tests/fixtures/langchain_json.log
cargo run -- smoketest tests/fixtures/crewai_logfmt.log
cargo run -- smoketest tests/fixtures/openai_chat_completions.log
cargo run -- smoketest tests/fixtures/react_agent.log
```

### Expected output (mcp_session.log, all switches on)

```
┌─ Smoke test ──────────────────────────────────────────────────
│ fixture     : tests/fixtures/mcp_session.log
│ target_id   : smoketest
│ content_len : ~800 bytes / 17 lines
├─ Wiring snapshot ─────────────────────────────────────────────
│ entity_extraction_enabled = true
│ min_entities_for_persist  = 1
│ graph_writer_enabled      = true
│ vector_writer_enabled     = true
│ embedding_enabled         = false
├─ Pipeline result ─────────────────────────────────────────────
│ sample_hash       = <sha256>
│ sample_was_new    = true
│ entities          = 17     (every line is an McpEvent)
│ relations         = N      (request ↔ response edges)
├─ Auxiliary collections (filtered by sample_hash) ─────────────
│ entity_edges      = N
│ prov_relations    = >N     (entity attribution + relation triples)
│ otel_spans        = 17
│ embeddings (c+b)  = 1      (behavioral only, content skipped)
└───────────────────────────────────────────────────────────────
```

If a row is unexpectedly zero, the subcommand prints a diagnostic note
explaining the most likely cause (e.g. `GRAPH_WRITER_ENABLED=false`,
`entities < min_entities_for_persist`, etc.).

## 3. Verify via MongoDB

```bash
mongosh log_samples --eval '
  print("sample_metadata: " + db.sample_metadata.countDocuments());
  print("entity_edges:    " + db.entity_edges.countDocuments());
  print("prov_relations:  " + db.prov_relations.countDocuments());
  print("otel_spans:      " + db.otel_spans.countDocuments());
  print("content_embeds:  " + db.content_embeddings.countDocuments());
  print("behavioral_emb:  " + db.behavioral_embeddings.countDocuments());
'
```

To inspect a single sample's full lineage:

```bash
SAMPLE_HASH=<from-smoketest-output>
mongosh log_samples --eval "
  printjson(db.sample_metadata.findOne({sample_hash: '$SAMPLE_HASH'}));
  print('--- entity_edges ---');
  db.entity_edges.find({sample_hash: '$SAMPLE_HASH'}).forEach(printjson);
  print('--- prov_relations ---');
  db.prov_relations.find({sample_hash: '$SAMPLE_HASH'}).forEach(printjson);
  print('--- otel_spans ---');
  db.otel_spans.find({sample_hash: '$SAMPLE_HASH'}).forEach(printjson);
"
```

## 4. Verify via API

In a separate terminal:

```bash
cd logflayer
cargo run                               # starts the API on :8080
```

Then:

```bash
curl 'http://localhost:8080/api/v1/metadata?limit=5'  | jq
curl 'http://localhost:8080/api/v1/relations?limit=5' | jq
curl 'http://localhost:8080/api/v1/prov?limit=5'      | jq
curl 'http://localhost:8080/api/v1/spans?limit=5'     | jq

# Or scope to the smoke-test sample:
curl "http://localhost:8080/api/v1/relations?sample_hash=$SAMPLE_HASH" | jq
curl "http://localhost:8080/api/v1/prov?sample_hash=$SAMPLE_HASH"      | jq
curl "http://localhost:8080/api/v1/spans?sample_hash=$SAMPLE_HASH"     | jq
```

## 5. Verify via UI

```bash
cd logflayer-ui
npm run dev                             # http://localhost:5173
```

Then in the browser, open the four UpsideGate views and confirm the
smoke-test sample appears in each:

- **Entities** — table fills with `McpEvent` rows; `semantic_role` shows
  `mcp_request` / `mcp_response`; tool names (`web_search`, `calculator`,
  `read_file`) appear in the Tool / MCP column.
- **Relations** — table shows `TRIGGERED_BY` / `RESPONDED_TO` edges with
  source and target entities resolved by name.
- **PROV** — triples render with `wasGeneratedBy` / `wasDerivedFrom` /
  `wasAttributedTo` predicates. Subjects/objects strip the `ug:entity:`
  URI prefix so labels resolve.
- **Spans** — waterfall shows `CLIENT`-kind bars for tool calls; status
  badges show `UNSET` (or `OK` once Phase F lands real timestamps).

## Common failures

| Symptom                                     | Likely cause                                                    |
| ------------------------------------------- | --------------------------------------------------------------- |
| `entity_edges = 0` despite entities > 0     | `GRAPH_WRITER_ENABLED=false`                                    |
| `embeddings = 0` despite entities > 0       | `VECTOR_WRITER_ENABLED=false`                                   |
| `entities = 0` from an obviously-agentic log | `ENTITY_EXTRACTION_ENABLED=false`                               |
| All auxiliary collections empty             | `entities < ENTITY_EXTRACTION_MIN_ENTITIES` (default 1)         |
| `content_embeddings = 0`, behavioral = 1    | `EMBEDDING_ENABLED=false` or missing `EMBEDDING_API_KEY`        |
| API returns 500 on `/api/v1/relations`      | `entity_edges` collection missing — re-run with graph writer on |
| UI views show empty state                   | Sample inserted before frontend store loaded — refresh the page |

## Re-running

The smoke test is idempotent: `compute_sample_hash` is deterministic over
content + target_id + source_label, so re-running with identical inputs
upserts at the metadata layer and is safe. To start fresh:

```bash
mongosh log_samples --eval '
  db.sample_metadata.deleteMany({target_id: "smoketest"});
  db.entity_edges.deleteMany({});      // optional — drops everything
  db.prov_relations.deleteMany({});
  db.otel_spans.deleteMany({});
  db.content_embeddings.deleteMany({});
  db.behavioral_embeddings.deleteMany({});
  db.smoketest.drop();                 // the per-target samples collection
'
```
