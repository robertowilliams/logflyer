# Stages 11–13 — adversarial review

Review of the task-correlation, actor-node and task-intent work after it shipped.
Two independent read-only passes over the pipeline modules, the repository layer,
the API and the frontend, with every claim reproduced against the bundled
fixtures before being accepted.

Fifteen findings. Nine were fixed; six are recorded below as known limitations
with the reasoning for leaving them.

---

## 1. What was fixed

### 1.1 A sample containing two tasks was confidently mislabelled — *critical*

`crewai_logfmt.log` carries `task_id=task-1` (researcher) on line 6 and
`task_id=task-2` (writer, then reviewer) on line 12. `correlate` took the first
value and stamped it onto every entity in the sample, reporting
`is_real_boundary() == true` while doing it.

Half the sample was therefore attributed to the wrong task, and it propagated:
`actors.task_ids` recorded the writer as a participant in `task-1`, and
`task-1`'s `sample_hashes` gained a sample that was half someone else's work.

This is worse than the sample fallback, which is at least labelled as a guess.
The module header named "two tasks landing in one sample merge into one" as the
problem it existed to solve; the code did not solve it, it relabelled the merge
with a specific wrong id.

**Fix.** A key qualifies as a sample-wide boundary only when the whole sample
agrees on one value for it. Multi-valued keys are recorded in
`TaskCorrelation::spanning_keys`, logged at `warn`, and skipped in favour of a
coarser key the sample does agree on — `crew_id` for the crewai fixture.

### 1.2 Task ids depended on where the sampler cut — *critical*

`langchain_json.log` carries `session_id` on two lines and `run_id` on exactly
one: line 18, the last. Precedence was applied globally, so `run_id` — which
outranks `session_id` — won on the strength of a single incidental late
occurrence.

The mislabelling was not the real damage. **Instability** was: a sample cut at
line 17 correlated to the session, one cut at line 18 to the run, so two
overlapping samples of a single agent run landed in two disjoint tasks with no
way to join them. The task id was an artifact of collection, which is precisely
what this module exists to remove.

**Fix.** Coverage decides — the key accounting for the most lines wins, with
`CORRELATION_KEYS` order breaking ties. Coverage is stable under re-sampling in
a way that first-hit is not.

Both of the fixture tests that covered this correlator encoded the buggy
behaviour as expected. They were rewritten rather than kept green.

### 1.3 Vector search returned the query's own sample — *important*

`search_embeddings` built its filter with two `Document::insert` calls. Both can
target `sample_hash`, and `insert` is a map insert, so the target scope silently
replaced the self-exclusion:

```rust
filter.insert(key_field(kind), doc! { "$ne": hash });   // "sample_hash"
filter.insert("sample_hash",   doc! { "$in": hashes }); // overwrites it
```

Any content or behavioral search passing `target_id` therefore returned the
query's own sample as hit #1 at score 1.0 despite `include_self: false`. Only
`kind: "task"` escaped, because its key field is `task_id` and did not collide.

The two existing tests each exercised one argument with the other set to `None`,
so neither could see it. Conditions are now collected and combined with `$and`,
and `test_search_embeddings_excludes_self_while_scoped_to_a_target` covers the
combination (verified to fail against the old filter).

### 1.4 Task intent embedded whole JSON lines — *important*

`extracted_fields` holds a line's top-level keys and is not flattened, so the
ordinary shape of an LLM log —
`{"message":{"content":[{"type":"text","text":"…"}]}}` — matched no text field:
`message` exists but is an object. `usable_text` then fell through to
`raw_text`, making the task's stated goal the entire JSON line, timestamps and
token counts included.

It cleared `MIN_INTENT_CHARS` so nothing rejected it, was written by
`set_task_intent_if_absent` where the first writer wins permanently, and was
then embedded and indexed. The function's own doc comment says `raw_text` is
exactly what must not be embedded; the common case bypassed the guard.

**Fix.** The fallback now applies only to unstructured lines. A JSON line whose
text is nested yields no intent — better than a permanent wrong one that
poisons search.

### 1.5 One participant described twice was counted twice — *important*

`candidates` emits an Agent for `model_id` and another for `agent_id`/`agent`.
When both carry the same string — `agent=claude-3-opus model=claude-3-opus`, an
agent named after its model — both resolved to one `actor_id`, so `event_count`
incremented twice for one event and two `RelationEdge`s were emitted with an
**identical** `relation_id`. Those rode in `metadata.relations` and inflated the
task's `relation_count`, which is `$inc`'d onto the `TaskRecord` and persisted.

Fixed by deduplicating per event. The existing test used `model_id == tool_name`
— different kinds — and so could not catch it.

### 1.6 The two placeholder lists had drifted — *important*

`PLACEHOLDERS` (correlator) and `PLACEHOLDER_NAMES` (actors) each held entries
the other lacked:

- `undefined` was missing from the correlator. It is what JavaScript emits for a
  missing field and so the most likely placeholder in practice, which meant
  `session_id=undefined` became a *real* boundary merging every service that
  ever emitted one — exactly the failure the list existed to prevent.
- `<nil>` was missing from the actor list, so a Go log's `tool_name=<nil>`
  produced a Skill node literally named `<nil>`.

Now one shared `PLACEHOLDER_VALUES`, with a correlation-only extension for
values that are legitimate *names* but not legitimate *keys* (`default`,
`test`).

`"0"` was **removed**. Rejecting it made a zero-indexed `run_id=0` fall back to
sample scope while runs 1..n correlated normally — inconsistent grouping inside
one workload. The concern it stood in for was low entropy, now handled properly:
see 1.7.

### 1.7 Low-entropy correlation values merged unrelated sources — *important*

`derive_task_id` hashed only `(key_name, key_value)`, deliberately unscoped so a
task spanning several samples reassembles. The same property made `run_id=1`
dangerous: every system that numbers runs from one emits it, and hashing that
unscoped merged all of them into a single task. `TaskRecord.target_ids` is a
`Vec` and documents the multi-source state as a *feature*, so there was no way
to distinguish a genuinely cross-source task from a collision.

**Fix.** `derive_task_id` takes an optional scope. Values that pass
`is_globally_unique` (≥ 8 chars and not a bare number) stay global, so real
cross-source tasks still stitch together; anything shorter or purely numeric is
scoped to its `target_id`. Grouping still works where it is meaningful, and run
1 over here no longer collides with run 1 over there.

### 1.8 `/tasks` and `/actors` accepted any limit — *important*

Both took a bare `i64` straight to `FindOptions::limit`. Two values were
actively dangerous rather than merely large:

- `limit=0` — Mongo's find command reads zero as **no limit**, so the
  friendliest-looking value in the API returned the entire collection.
- `limit=-1&page=3` — flows into `skip(page * limit as u64)`, computing
  `3 * u64::MAX`, which panics in a debug build and wraps in release.

Now clamped to `1..=500`. `/search` already enforced its ceiling twice over;
these are the endpoints backed by a `count_documents` plus a sort the indexes
cannot always serve, so they were the cheapest in the API to abuse.

The same hole exists on the pre-existing `/metadata`, `/relations`, `/prov` and
`/spans` — the new endpoints copied it rather than introducing it. Not fixed
here to keep this change scoped; see 2.7.

### 1.9 Paths left actor endpoints unhydrated — *minor*

`graph_path` called `fetch_entities_by_ids` where `traverse_graph` correctly
called `fetch_graph_nodes_by_ids`. Actor edges run event → actor, so an actor is
an ordinary path sink; resolving only against `sample_metadata.entities` left it
present in `node_ids` but missing from `entities`. The frontend asserts the
opposite, hard-coding `unresolved_node_ids: []` with the comment "A path's nodes
are hydrated by definition."

Also fixed the TypeScript that made the same wrong assumption:
`GraphTraversal.entities` and `GraphPath.entities` were declared
`EntityRecord[]` but have carried mixed `GraphNode[]` since Stage 12. It
typechecked and worked at runtime because the component branches on `isActor`,
but any new consumer reading `.entity_type` off an actor would get `undefined`
with no compile error. Added the missing `TaskRecord`, `EmbeddingKind`,
`ScoredHit`, `SearchResponse` and `TaskGraph` mirrors, and the Stage 11 fields
on `EntityRecord` and `SampleMetadata`.

---

## 2. Known limitations — not fixed, and why

### 2.1 A sample spanning several tasks still becomes one task

1.1 stops the sample being labelled with the *wrong* task, but it still produces
one `TaskRecord`. The correct model is to partition the sample and let it
contribute to several tasks, which means `SampleMetadata.task_id` becomes plural
and the upsert, actor attribution and intent extraction all run per partition.

Left out because it is a schema change to a shipped collection, and the honest
fallback plus `spanning_keys` makes the current behaviour truthful rather than
merely tolerable. `spanning_keys` is the signal that tells you when it matters.

### 2.2 Actor ids are not scoped to a target

`derive_actor_id` hashes `(kind, name)` only, so customer A's `search` tool and
customer B's unrelated `search` tool are one node, sharing an `event_count` and
a `sample_hashes` list. For generic names (`search`, `query`, `assistant`) this
is near-certain in any multi-tenant deployment.

Not fixed because the trade-off runs the other way from 1.7: an actor is a thing
*in a system*, and in a single-tenant deployment — which is what the sampling
targets currently are, files and hosts of one estate — scoping by target would
over-split the node you most want unified. Revisit if LogFlayer becomes
multi-tenant.

Related and also unfixed: `normalise` trims but does not case-fold, so `GPT-4o`
and `gpt-4o` are two nodes. That fragments rather than merges, so it is the safer
direction to be wrong in.

### 2.3 `crew_id` and `session_id` are collection-level boundaries

A CrewAI `Crew` constructed once in a long-running service and reused keeps a
stable id, so every task it ever ran merges into one `TaskRecord` with an
unbounded `sample_hashes` list and a single `intent_text` from whichever sample
arrived first. A multi-day chat `session_id` has the same shape.

`task_id_source` records the granularity, so the coarseness is visible rather
than hidden. Distinguishing "one crew, many tasks" from "one crew, one task"
needs 2.1.

### 2.4 Stage 12 only sees entities, so named agents are invisible

`actor_extractor::extract` reads `EntityRecord`s. In `crewai_logfmt.log`,
`agent=researcher` / `writer` / `reviewer` appear only on lines matching no
`TYPE_PATTERNS` entry, so none become entities — a three-agent crew produces
**zero** named Agent nodes. The only Agents are the two models picked up from
the invocation lines.

This is the same mistake Stage 11 already learned and documents: correlation
keys routinely live on lines that never become entities. The fix is to scan
whole content the way `correlate` does. Deferred because it changes what the
`actors` collection contains for every existing sample, and wants doing together
with 2.1.

### 2.5 Backfill leaves samples in an inconsistent state

`backfill.rs` keeps only `PipelineOutput::metadata`, discarding
`task_correlation`, `actors` and `task_intent`. A backfilled sample therefore has
`task_id` and `task_id_source` populated on its metadata — so it *claims* task
membership — but no `tasks` document, no `actors` documents and no
`task_embeddings`. It is invisible to `GET /api/v1/tasks` and to `kind: "task"`
search.

That is worse than absent: present and inconsistent. `--reprocess_stale`
compounds it by deleting and rebuilding metadata while leaving orphaned `tasks`
and `actors` rows behind.

This extends the pre-existing `UPSIDEGATE_PLAN.md` §7 backfill gap to three more
collections. Recorded here rather than fixed because backfill needs one coherent
pass over all of it.

### 2.6 A failed task upsert does not self-heal

`upsert_task_for_sample` warns and returns on error, leaving `sample_metadata`
with a populated `task_id` and no `tasks` row. `run_preprocessing` only runs on
`StoreOutcome::Inserted`, so the sample is never reprocessed, and backfill does
not write tasks (2.5) — the hole is permanent.

Actor records have the same shape: `upsert_actors_for_sample` logs and
`continue`s per actor, while `persist_lineage` writes the edges regardless, so
`entity_edges` can point at an `actor_id` with no row behind it. `traverse_graph`
reports those in `unresolved_node_ids`, which is a diagnostic, not a repair.

Both need a reconciliation path, which is the same work as 2.5.

### 2.7 Smaller items, recorded

- **Check-then-act race.** `task_sample_counted` / `actor_sample_counted` are
  read before the upsert decides whether to `$inc`. Two processes handling the
  same `(task_id, sample_hash)` can both increment. Narrow — the live loop is
  concurrent over targets but sequential within one, and `store_sample` dedupes
  by hash first. The bigger practical hole in the same guard is that
  reprocessing a sample whose extraction *changed* takes the `already_counted`
  path and keeps stale counters forever.
- **`fetch_task_graph` truncates to the oldest 500 samples.** Deterministic and
  honestly flagged via `truncated`, but for an audit payload, discarding the end
  of a long task is close to the worst choice. There is no `page`/`offset` on
  the endpoint to reach the rest. Its `actors` are also fetched for the whole
  task, so a truncated response contains participants whose edges were excluded.
- **`first_seen` / `last_seen` are ingest time, not event time.** So
  `fetch_tasks_page` and `fetch_actors_page` sort by when a log happened to be
  *processed*, and a reprocessed old sample jumps to the top. For a backfill the
  ordering is meaningless.
- **No index-ensure on the tasks/actors read paths.** Those only run from the
  upserts, so an API-only process sorts in memory and will hit Mongo's 32 MB
  sort cap on a large collection.
- **Unclamped `limit` on `/metadata`, `/relations`, `/prov`, `/spans`** — same
  hole as 1.8, pre-existing, same fix.
- **No UI surfaces Stages 11 or 13 at all.** There is no client method, route or
  view for `/tasks`, `/tasks/:id`, `/tasks/:id/graph`, `/actors` or `/search`.
  Stage 12 is half-visible: `RelationGraph.vue` renders an actor node correctly
  *if* a traversal happens to reach one, but there is no actor browser and no
  entry point from a task. The search-then-audit loop these stages were built
  for cannot be walked from the interface.

---

## 3. The pattern

Every finding above is the same failure mode, and it is the one this codebase
keeps producing: **output that looks right and is quietly wrong**. Not crashes,
not empty results — plausible values that survive inspection.

A confidently-labelled wrong task id. A similarity search that ranks your own
document first. A task goal made of token counts. A relation count inflated by
duplicate edges. Each of these renders fine in a UI and reads fine in a database
dump.

Unit tests do not catch this class, because assertions get written against the
same wrong assumption as the code. Three of the bugs above had passing tests
that *encoded* them:

- `precedence_holds_across_lines_not_just_within_one` tested the hijack.
- `crewai_fixture_correlates_on_its_own_task_id` asserted the wrong attribution.
- `test_task_accumulates_across_samples` pinned `run_id` as correct.

What caught them was checking the fixtures directly — `grep`ping
`crewai_logfmt.log` for how many distinct `task_id` values it actually contains,
and `langchain_json.log` for how many lines actually carry `run_id`. Two shell
commands, and both critical findings fell out immediately.

The standing rules, reaffirmed:

1. **Reproduce before fixing.** Every fix above has a test verified to fail
   against the old code.
2. **Assert against the data, not against the design.** Read the fixture; count
   the values.
3. **`cargo test --all-targets`, never `--lib`.** The integration tests went
   uncompiled for eight days behind a green `--lib`.
4. **A passing test proves the code matches the test's assumption.** Nothing
   more.
