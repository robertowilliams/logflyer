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
│ content_len : 2902 bytes / 19 lines
├─ Wiring snapshot ─────────────────────────────────────────────
│ entity_extraction_enabled = true
│ min_entities_for_persist  = 1
│ graph_writer_enabled      = true
│ vector_writer_enabled     = true
│ embedding_enabled         = false
├─ Pipeline result ─────────────────────────────────────────────
│ sample_hash       = 9cf60df0…4e5b04
│ sample_was_new    = true
│ entities          = 19     (every line is an McpEvent)
│ relations         = 27     (19 PART_OF + 8 RESPONDED_TO)
├─ Auxiliary collections (filtered by sample_hash) ─────────────
│ entity_edges      = 27
│ prov_relations    = 46     (entity attribution + relation triples)
│ otel_spans        = 19
│ embeddings (c+b)  = 1      (behavioral only, content skipped)
└───────────────────────────────────────────────────────────────
```

If a row is unexpectedly zero, the subcommand prints a diagnostic note
explaining the most likely cause (e.g. `GRAPH_WRITER_ENABLED=false`,
`entities < min_entities_for_persist`, etc.).

### All fixtures, verified July 25 2026

| Fixture                     | entities | relations | prov | spans | embeds |
| --------------------------- | -------- | --------- | ---- | ----- | ------ |
| `mcp_session.log`           | 19       | 27        | 46   | 19    | 1      |
| `langchain_json.log`        | 16       | 33        | 41   | 16    | 1      |
| `crewai_logfmt.log`         | 8        | 16        | 21   | 8     | 1      |
| `openai_chat_completions.log` | 7      | 16        | 22   | 7     | 1      |
| `react_agent.log`           | 9        | 23        | 26   | 9     | 1      |
| `bedrock_multiline.log`     | 5        | 13        | 18   | 5     | 1      |
| `nginx_access.log`          | 0        | 0         | 0    | 0     | 0      |

`nginx_access.log` yielding zero is correct — it is a non-agentic access log
and exists to prove the pipeline does *not* invent entities for one.

### Span status and timestamp coverage

Both depend on what the log actually contains, so "not all spans" is usually
the right answer rather than a bug:

| Fixture                     | spans with a timestamp | notes                                    |
| --------------------------- | ---------------------- | ---------------------------------------- |
| `openai_chat_completions.log` | 7/7                  | one `OK` span from `choices[0].finish_reason`; 2 have real durations |
| `langchain_json.log`        | 16/16                  | `time` field                             |
| `crewai_logfmt.log`         | 8/8                    | logfmt `time=`                           |
| `bedrock_multiline.log`     | 3/5                    | leading `2026-04-26 10:00:02`; continuation lines have none; 1 `ERROR` from plain-text severity |
| `react_agent.log`           | 1/9                    | only the bracketed header lines carry one; `Thought:` / `Action:` lines do not |
| `mcp_session.log`           | 0/19                   | raw JSON-RPC has no timestamp field at all; 1 `ERROR` from the JSON-RPC error envelope |

A span with no timestamp gets `start_time_unix_nano = 0` and sorts first in the
waterfall. That is a property of the input, not a pipeline failure.

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

### Entity lookup and graph traversal

```bash
# Grab a real entity id from an entity-to-entity edge. Do NOT use the first
# relation blindly — relations[0] is a PART_OF edge whose target is the sample's
# OTel trace id, not an entity.
read CHILD PARENT <<<"$(curl -s \
  "http://localhost:8080/api/v1/relations?sample_hash=$SAMPLE_HASH&relation_type=RESPONDED_TO&limit=1" \
  | jq -r '.records[0] | "\(.source_entity_id) \(.target_entity_id)"')"

# Both id spellings resolve; an unknown id is a 404.
curl "http://localhost:8080/api/v1/entities/$CHILD"              | jq '.entity.entity_type'
curl "http://localhost:8080/api/v1/entities/ug%3Aentity%3A$CHILD" | jq '.entity.entity_id'

# RESPONDED_TO runs child -> parent, so walk downstream from the child.
curl "http://localhost:8080/api/v1/graph/downstream/$CHILD?depth=2" | jq \
  '{node_count, edge_count, depth_reached, truncated, unresolved_node_ids}'
curl "http://localhost:8080/api/v1/graph/upstream/$PARENT?depth=2"  | jq '.node_count'

# Directed: child -> parent resolves, the reverse does not.
curl "http://localhost:8080/api/v1/graph/path?from=$CHILD&to=$PARENT" | jq '{found, hop_count}'
curl "http://localhost:8080/api/v1/graph/path?from=$PARENT&to=$CHILD" | jq '{found, truncated}'
```

### Tasks and actors (Stages 11–12)

Both are off by default. Enable with `TASK_CORRELATION_ENABLED=true` and
`ACTOR_NODES_ENABLED=true`, then re-run the fixtures.

```bash
# Which tasks exist, and was each boundary real or a fallback?
mongosh log_samples --eval '
  db.tasks.find({}, {_id:0, task_id_source:1, correlation_key:1, sample_hashes:1}).forEach(printjson)'

# The participants, and how far each reaches across samples.
mongosh log_samples --eval '
  db.actors.find({}, {_id:0, kind:1, name:1, event_count:1, sample_hashes:1}).forEach(printjson)'
```

Expected on the bundled fixtures: langchain correlates on `run_id`, crewai on
`task_id`, bedrock on `request_id`; mcp and react fall back with
`task_id_source: "sample"` because those logs carry no correlation key. Actors
span samples — `web_search` is one node across three of them.

**The audit query this exists for** — which events used a given skill, across
every sample:

```bash
SKILL=$(mongosh --quiet log_samples --eval 'print(db.actors.findOne({name:"web_search"}).actor_id)')
curl -s "http://localhost:8080/api/v1/graph/upstream/$SKILL?depth=1" | jq \
  '{node_count, unresolved_node_ids, edges: [.edges[].relation_type] | unique}'
```

`unresolved_node_ids` must be **empty**. Actor nodes live in `actors` rather than
in `sample_metadata.entities`, so traversal resolves both — a non-empty list here
means an edge is pointing at an actor whose record was never written.

Note actors are deliberately **not** entities: `otel_builder` emits one span per
entity, so making them entities would fabricate a span for every agent and tool.
A quick check that this holds — the two counts must match:

```bash
mongosh log_samples --eval '
  print(db.otel_spans.countDocuments());
  printjson(db.sample_metadata.aggregate([{$unwind:"$entities"},{$count:"n"}]).toArray())'
```

### Task intent and intent search (Stage 13)

A task's **intent** is the sentence stating what it was for, embedded on its own
so that searching returns tasks with a similar *purpose* rather than a similar
amount of logging.

```bash
mongosh log_samples --eval '
  db.tasks.find({}, {_id:0, task_id_source:1, intent_text:1}).forEach(printjson)'
```

Expected on the bundled fixtures — note that **two of five decline**, which is
the intended behaviour rather than a gap:

| Fixture | Intent |
|---|---|
| `openai_chat_completions.log` | "You are a helpful AI assistant specialized in research and web search." |
| `langchain_json.log` | "I need to search for the current weather" |
| `react_agent.log` | "I need to find the current weather in New York City and then convert…" |
| `crewai_logfmt.log` | *none* |
| `mcp_session.log` | *none* |

crewai declines because its only reasoning-role lines are `crew.kickoff called`
and `crew.kickoff finished` — lifecycle events, not goals. Indexing one would
make every crewai run look identical and quietly ruin the search, so an intent is
only accepted from a reasoning entity carrying an explicit marker (`Thought:`,
`Plan:`, `Goal:`, …), which is then stripped. **No intent is better than a
misleading one.**

Intent search itself needs an embedding provider:

```bash
export EMBEDDING_ENABLED=true EMBEDDING_API_KEY=sk-...
cargo run -- smoketest tests/fixtures/openai_chat_completions.log

# Then "find tasks like this one":
curl -s -X POST http://localhost:8080/api/v1/search \
  -H 'Content-Type: application/json' \
  -d '{"task_id":"<id>","kind":"task","limit":5}' | jq '.hits[] | {score, task_id}'
```

Task search keys on `task_id`, **not** `sample_hash` — a task spans samples, so
there is one intent vector per task. Passing `sample_hash` with `kind: "task"` is
a 400 rather than a silent empty result.

⚠️ **Not yet verified end to end:** the embedding provider call itself. Everything
either side of it — intent selection, record construction, storage, keying,
ranking, self-exclusion — is covered by integration tests using an injected
vector. What remains untested is that the model returns a usable vector, which
needs a real API key.

### Vector search

Embeddings are keyed per **sample**, so this finds similar *samples* — the
behavioural-clustering question, not "which log line resembles this line".

```bash
# Search by example: a sample's own vector, minus itself.
curl -s -X POST http://localhost:8080/api/v1/search \
  -H 'Content-Type: application/json' \
  -d "{\"sample_hash\":\"$SAMPLE_HASH\",\"kind\":\"behavioral\",\"limit\":5}" | jq

# include_self:true puts the query sample back in — it scores exactly 1.0, which
# is the quickest way to confirm the endpoint is scoring correctly.
curl -s -X POST http://localhost:8080/api/v1/search \
  -H 'Content-Type: application/json' \
  -d "{\"sample_hash\":\"$SAMPLE_HASH\",\"include_self\":true,\"limit\":3}" \
  | jq '.hits[] | {score, sample_hash}'
```

Expected on the bundled fixtures: the two MCP samples score ~0.97 against each
other, and structurally different agent runs (langchain, crewai) sit near 0.43.
That gap is the endpoint working — pure-MCP sessions cluster, mixed
agent/tool/completion runs do not.

`kind` defaults to `behavioral`. Asking for `content` returns **400** unless
`EMBEDDING_ENABLED=true` and an API key are set, because that collection is
otherwise empty — the error says so rather than returning zero hits.

**Reading `scored` / `skipped`.** `skipped` counts candidates whose vector could
not be compared, which in practice means a dimensionality mismatch — behavioral
vectors are 36-dimensional, content 1536. So:

| Result | Meaning |
| ------ | ------- |
| `scored > 0`, `hits` empty | nothing is similar |
| `scored: 0`, `skipped > 0` | your query vector is the wrong size |
| `scored: 0`, `skipped: 0`  | the filter matched no samples at all |

Two things to know about traversal:

- **`PART_OF` edges are excluded by default.** Their `target_entity_id` holds the
  sample's OTel trace id rather than an `entity_id`, and every entity has one, so
  following them would add the same unlabelled dead-end node to every walk. Pass
  `include_structural=true` to opt in; the trace then shows up in
  `unresolved_node_ids`, because it has no entity record to hydrate.
- **`found: false` and `truncated: true` are different answers.** The first means
  the search finished and there is no path; the second means it hit a size limit
  and stopped, so the pair may still be connected.

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
- **Relations** — table shows `PART_OF` / `RESPONDED_TO` edges with source and
  target entities resolved by name. The force graph below it should be
  *connected*, not a scatter of isolated nodes. Double-click a node for
  Upstream / Downstream traversal; the toolbar's route button starts a
  two-click shortest-path pick.
- **PROV** — triples render with `wasGeneratedBy` / `wasDerivedFrom` /
  `wasAttributedTo` predicates. Subjects/objects strip the `ug:entity:`
  URI prefix so labels resolve. References to entities outside the loaded
  sample resolve through `/api/v1/entities/:id` rather than showing `?`.
- **Spans** — waterfall shows `CLIENT`-kind bars for tool calls. Status badges
  should include at least one non-`UNSET` value on `mcp_session.log` (an
  `ERROR` from the JSON-RPC error envelope) and on
  `openai_chat_completions.log` (an `OK` from `finish_reason`). Bars reflect
  real durations wherever the log carried a timestamp — see the coverage table
  above for which fixtures do.

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
| Traversal returns an unlabelled node        | An edge points at something with no entity record; check `unresolved_node_ids` |
| Graph view is all isolated nodes            | Sample produced only `PART_OF` edges — no relation rule matched its shape |
| All spans `UNSET` with zero timestamps      | Log has no timestamp field and no leading timestamp in the line (e.g. raw JSON-RPC) |

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
