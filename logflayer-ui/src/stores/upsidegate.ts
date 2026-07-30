import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import { client } from '../api/client'
import type {
  SampleMetadata, EntityRecord, RelationEdge, ProvTriple, OtelSpan,
  EntityType, RelationType, SpanKind, SpanStatusCode,
  GraphTraversal, GraphPath,
  TaskRecord, TaskStatus, TaskGraph, ActorRecord, ActorKind, GraphNode, ScoredHit,
} from '../types'

export const useUpsidegateStore = defineStore('upsidegate', () => {
  // ── State ──────────────────────────────────────────────────────────────────
  const metadataList  = ref<SampleMetadata[]>([])
  const metadataTotal = ref(0)
  const selected      = ref<SampleMetadata | null>(null)
  const loading       = ref(false)
  const error         = ref<string | null>(null)

  // ── Server-fetched UpsideGate output (Phase 6 endpoints) ──────────────────
  // Populated opportunistically by `fetchMetadataByHash` when the backend has
  // the graph writer enabled.  Failures are tolerated silently — the existing
  // client-side computed `relations` / `provTriples` / `otelSpans` below act
  // as the fallback so views never see empty state when only client synthesis
  // is available.
  //
  // Now strongly typed against the canonical backend serialisation (see
  // `types/index.ts`).
  const serverRelations    = ref<RelationEdge[]>([])
  const serverProvTriples  = ref<ProvTriple[]>([])
  const serverSpans        = ref<OtelSpan[]>([])
  /** True when the most-recent /relations|/prov|/spans fetch completed
   *  successfully.  Lets views detect "graph writer is enabled" at runtime
   *  without needing a separate config probe. */
  const serverDataAvailable = ref(false)

  // ── Filters ────────────────────────────────────────────────────────────────
  const filterTargetId    = ref('')
  const filterEntityType  = ref<EntityType | ''>('')
  const filterRelationType = ref<RelationType | ''>('')

  // ── Derived views on selected metadata ───────────────────────────────────
  const entities = computed<EntityRecord[]>(() => selected.value?.entities ?? [])
  const relations = computed<RelationEdge[]>(() => selected.value?.relations ?? [])

  // ProvTriples — prefer server-fetched when available, else synthesise
  // client-side from relations.  The synthesis is lossy (no `created_at`,
  // crude predicate mapping) so views should treat `serverDataAvailable`
  // as the cue to switch to richer rendering.
  const provTriples = computed<ProvTriple[]>(() => {
    if (serverProvTriples.value.length > 0) return serverProvTriples.value
    if (!selected.value) return []
    const sampleHash = selected.value.sample_hash
    return relations.value.map(r => ({
      subject:     r.source_entity_id,
      predicate:   relationTypeToProvPredicate(r.relation_type),
      object:      r.target_entity_id,
      sample_hash: sampleHash,
      created_at:  r.created_at ?? '',
    }))
  })

  // OTel spans — prefer server-fetched when available, else synthesise from
  // the entity list.
  const otelSpans = computed<OtelSpan[]>(() => {
    if (serverSpans.value.length > 0) return serverSpans.value
    if (!selected.value) return []
    const seen = new Set<string>()
    const spans: OtelSpan[] = []
    for (const e of entities.value) {
      if (seen.has(e.span_id)) continue
      seen.add(e.span_id)
      const statusCode: SpanStatusCode = 'UNSET'
      spans.push({
        trace_id:             e.trace_id,
        span_id:              e.span_id,
        parent_span_id:       e.parent_span_id ?? null,
        name:                 `${e.semantic_role}:${e.entity_type}`,
        kind:                 entityTypeToSpanKind(e.entity_type),
        start_time_unix_nano: 0,
        end_time_unix_nano:   0,
        attributes: {
          'entity.type': e.entity_type,
          'entity.id':   e.entity_id,
          ...(e.mcp_server_id ? { 'mcp.server_id': e.mcp_server_id } : {}),
          ...(e.tool_name     ? { 'tool.name':      e.tool_name }     : {}),
          ...(e.model_id      ? { 'model.id':       e.model_id }      : {}),
        },
        status:      { code: statusCode },
        sample_hash: e.sample_hash,
      })
    }
    return spans
  })

  // ── Filtered entity/relation lists ────────────────────────────────────────
  const filteredEntities = computed(() => {
    if (!filterEntityType.value) return entities.value
    return entities.value.filter(e => e.entity_type === filterEntityType.value)
  })

  const filteredRelations = computed(() => {
    if (!filterRelationType.value) return relations.value
    return relations.value.filter(r => r.relation_type === filterRelationType.value)
  })

  // ── Entity type / relation type frequency counts ──────────────────────────
  const entityTypeCounts = computed(() => {
    const counts: Record<string, number> = {}
    for (const e of entities.value) {
      counts[e.entity_type] = (counts[e.entity_type] ?? 0) + 1
    }
    return counts
  })

  const relationTypeCounts = computed(() => {
    const counts: Record<string, number> = {}
    for (const r of relations.value) {
      counts[r.relation_type] = (counts[r.relation_type] ?? 0) + 1
    }
    return counts
  })

  // ── Actions ────────────────────────────────────────────────────────────────
  async function fetchMetadata(params: {
    target_id?: string; limit?: number; page?: number; worth_classifying?: boolean
  }) {
    try {
      loading.value = true; error.value = null
      const res = await client.getMetadata(params)
      metadataList.value  = res.records
      metadataTotal.value = res.total
    } catch (e: any) {
      error.value = e.message ?? 'Failed to fetch metadata'
      metadataList.value  = []
      metadataTotal.value = 0
    } finally {
      loading.value = false
    }
  }

  async function fetchMetadataByHash(hash: string) {
    try {
      loading.value = true; error.value = null
      const res = await client.getMetadataByHash(hash)
      selected.value = res.metadata
    } catch (e: any) {
      error.value = e.message ?? 'Failed to fetch metadata'
      selected.value = null
    } finally {
      loading.value = false
    }
    // Best-effort: pull server-side relations / prov / spans so views can
    // prefer them over the client-synthesised fallbacks.  Errors are
    // swallowed because the graph writer is opt-in on the server and a
    // 404 / empty page just means "fall back to client synthesis."
    // A traversal belongs to the sample it was launched from; carrying it into
    // a different sample would leave the graph showing unrelated entities.
    clearExpansion()
    if (selected.value) {
      void fetchServerOutputs(hash)
    } else {
      clearServerOutputs()
    }
  }

  /** Pull server-side relation edges, PROV triples, and OTel spans for the
   *  given sample.  All three calls run in parallel; partial failures leave
   *  the corresponding state empty without surfacing an error. */
  async function fetchServerOutputs(sampleHash: string) {
    const [relRes, provRes, spanRes] = await Promise.allSettled([
      client.getRelations({ sample_hash: sampleHash, limit: 500, page: 1 }),
      client.getProvTriples({ sample_hash: sampleHash, limit: 500, page: 1 }),
      client.getOtelSpans({ sample_hash: sampleHash, limit: 500, page: 1 }),
    ])

    serverRelations.value =
      relRes.status === 'fulfilled' ? (relRes.value.records as RelationEdge[]) : []
    serverProvTriples.value =
      provRes.status === 'fulfilled' ? (provRes.value.records as ProvTriple[]) : []
    serverSpans.value =
      spanRes.status === 'fulfilled' ? (spanRes.value.records as OtelSpan[]) : []

    // Consider data "available" when at least one collection had content —
    // i.e. the graph writer is enabled and has produced something.  An
    // all-empty result is indistinguishable from "graph writer disabled" so
    // we treat it as not-available and let the client-side fallback render.
    serverDataAvailable.value =
      serverRelations.value.length > 0 ||
      serverProvTriples.value.length > 0 ||
      serverSpans.value.length > 0
  }

  function clearServerOutputs() {
    serverRelations.value = []
    serverProvTriples.value = []
    serverSpans.value = []
    serverDataAvailable.value = false
  }

  // ── Graph traversal (server-side BFS) ─────────────────────────────────────
  // /relations is scoped to one sample; these follow `entity_edges` wherever
  // they lead.  When a traversal is active the graph view renders it instead of
  // the sample's own relations, so `expansion` acts as an overlay that
  // `clearExpansion` removes to return to the sample-scoped view.
  const expansion      = ref<GraphTraversal | null>(null)
  const expanding      = ref(false)
  const expansionError = ref<string | null>(null)
  /** What produced the current overlay. A resolved path reuses the same
   *  rendering path as a traversal, but describing it as a "downstream
   *  traversal" in the banner would be wrong, so track which it is. */
  const expansionKind  = ref<'traversal' | 'path'>('traversal')

  /** Edges the graph should draw: the active traversal if there is one,
   *  otherwise the selected sample's own relations.
   *
   *  The relation-type filter applies to both, so the control does not appear
   *  to stop working the moment a traversal is active. */
  const graphRelations = computed<RelationEdge[]>(() => {
    const source = expansion.value ? expansion.value.edges : relations.value
    if (!filterRelationType.value) return source
    return source.filter(r => r.relation_type === filterRelationType.value)
  })

  /** Entities the graph should label nodes with — the traversal's hydrated
   *  records when expanded, since they may span samples. */
  const graphEntities = computed<EntityRecord[]>(() =>
    expansion.value ? expansion.value.entities : entities.value,
  )

  /** Walk outbound edges from `entityId` ("what did this cause?").
   *  Accepts a bare id or a `ug:entity:` URI. */
  async function expandDownstream(entityId: string, depth = 2) {
    await runExpansion(() => client.getDownstream(entityId, depth))
  }

  /** Walk inbound edges into `entityId` ("what caused this?"). */
  async function expandUpstream(entityId: string, depth = 2) {
    await runExpansion(() => client.getUpstream(entityId, depth))
  }

  /** Resolve the shortest directed path between two entities.
   *  Returns the response so callers can distinguish "no path" (`found:
   *  false`) from a request failure (`null`). */
  async function findPath(from: string, to: string, maxDepth = 6): Promise<GraphPath | null> {
    try {
      expanding.value = true; expansionError.value = null
      const res = await client.getGraphPath(from, to, maxDepth)
      if (res.found) {
        // Reuse the expansion overlay so the path renders in the same graph.
        expansionKind.value = 'path'
        expansion.value = {
          root:          res.from,
          direction:     'downstream',
          depth_reached: res.hop_count,
          edges:         res.edges,
          entities:      res.entities,
          node_ids:      res.node_ids,
          node_count:    res.node_ids.length,
          edge_count:    res.edges.length,
          // A path's nodes are hydrated by definition — the server only returns
          // entities for the hops it resolved.
          unresolved_node_ids: [],
          truncated:     res.truncated,
        }
      } else if (res.truncated) {
        // The search hit a budget, so "no path" is not a safe conclusion.
        expansionError.value =
          'The search hit its size limit before finishing, so these entities may still be connected. ' +
          'Narrowing the relation-type filter will shrink the graph it has to walk.'
      } else {
        // Edges are directed and the search only follows them outward, so the
        // reverse pick is a genuinely different question — and a common mistake.
        expansionError.value =
          'No path found. Relations are directed, so try picking them the other way round.'
      }
      return res
    } catch (e: any) {
      expansionError.value = e.message ?? 'Path lookup failed'
      return null
    } finally {
      expanding.value = false
    }
  }

  async function runExpansion(fetcher: () => Promise<GraphTraversal>) {
    try {
      expanding.value = true; expansionError.value = null
      expansion.value = await fetcher()
      expansionKind.value = 'traversal'
    } catch (e: any) {
      // Leave any previous expansion in place — a failed expand should not
      // blank out a graph the user is already looking at.
      expansionError.value = e.message ?? 'Traversal failed'
    } finally {
      expanding.value = false
    }
  }

  /** Drop the traversal overlay and fall back to the sample-scoped view. */
  function clearExpansion() {
    expansion.value = null
    expansionError.value = null
    expansionKind.value = 'traversal'
  }

  /** Resolve a single entity that is not in the loaded sample.
   *  Returns `null` on 404 or error — callers fall back to showing the id. */
  async function resolveEntity(entityId: string): Promise<EntityRecord | null> {
    try {
      const res = await client.getEntity(entityId)
      return res.entity
    } catch {
      return null
    }
  }

  function selectMetadata(meta: SampleMetadata | null) {
    selected.value = meta
    clearExpansion()
    if (meta) {
      void fetchServerOutputs(meta.sample_hash)
    } else {
      clearServerOutputs()
    }
  }

  function clearError() { error.value = null }

  // ── Helpers ────────────────────────────────────────────────────────────────
  /** Maps a backend `RelationType` to the closest PROV-O predicate.
   *  Mirrors `prov_linker::build` in the Rust backend so client-side
   *  synthesis stays consistent with what the server would emit. */
  function relationTypeToProvPredicate(rt: RelationType): ProvTriple['predicate'] {
    const map: Record<RelationType, ProvTriple['predicate']> = {
      TRIGGERED_BY:   'wasGeneratedBy',
      GENERATED:      'wasGeneratedBy',
      INFORMED:       'used',
      FOLLOWED_BY:    'wasDerivedFrom',
      RESPONDED_TO:   'wasDerivedFrom',
      ASSEMBLED_FROM: 'wasDerivedFrom',
      PART_OF:        'wasAttributedTo',
      DELEGATED_TO:   'actedOnBehalfOf',
      // Stage 12 — event → actor. An event attributed to an agent is literally
      // PROV's `wasAttributedTo`; invoking a skill or reaching a resource is an
      // activity consuming something, which is `used`.
      PERFORMED_BY:      'wasAttributedTo',
      USED_SKILL:        'used',
      ACCESSED_RESOURCE: 'used',
    }
    return map[rt] ?? 'wasDerivedFrom'
  }

  /** Maps a backend `EntityType` to the OTel `SpanKind` chosen by
   *  `otel_builder::span_kind` in the Rust backend. */
  function entityTypeToSpanKind(et: EntityType): SpanKind {
    switch (et) {
      case 'prompt_event':                                   return 'PRODUCER'
      case 'completion_event':
      case 'tool_result_event':                              return 'CONSUMER'
      case 'tool_call_event':
      case 'retrieval_event':
      case 'mcp_event':                                       return 'CLIENT'
      case 'agent_step':
      case 'context_window':
      case 'unknown':
      default:                                               return 'INTERNAL'
    }
  }

  // ── Tasks and actors (Stages 11–14) ────────────────────────────────────────
  // Kept separate from the sample-centric state above: a task is not scoped to a
  // sample and routinely spans several, which is the whole point of Stage 11.

  const taskList     = ref<TaskRecord[]>([])
  const taskTotal    = ref(0)
  const selectedTask = ref<TaskRecord | null>(null)
  const taskGraph    = ref<TaskGraph | null>(null)
  const taskLoading  = ref(false)
  const taskError    = ref<string | null>(null)
  /** Drop tasks whose boundary was the sample fallback rather than a log key. */
  const realBoundariesOnly = ref(true)
  /** Narrow to one status (Stage 14) — `''` means no filter. */
  const filterTaskStatus = ref<TaskStatus | ''>('')

  const actorList  = ref<ActorRecord[]>([])
  const actorTotal = ref(0)
  const filterActorKind = ref<ActorKind | ''>('')

  /** Semantically similar tasks for the selected one. */
  const similarTasks   = ref<ScoredHit[]>([])
  const searching      = ref(false)
  const searchError    = ref<string | null>(null)
  /** True when the task simply has no embedding yet — not a failure. */
  const searchUnavailable = ref(false)

  /**
   * The task graph's nodes, in the shape `RelationGraph` wants.
   *
   * Events and actors arrive in separate arrays but are one node set: actor
   * edges run event → actor, so both endpoints must be resolvable or the
   * component falls back to rendering a truncated id.
   */
  const taskGraphNodes = computed<GraphNode[]>(() =>
    taskGraph.value ? [...taskGraph.value.entities, ...taskGraph.value.actors] : [],
  )

  /**
   * Relation types that group events rather than describe an interaction.
   *
   * `PART_OF` targets the OTel `trace_id`, which is neither an event nor a
   * participant — nothing resolves it, so `RelationGraph` would draw it as an
   * unlabelled hub with one spoke per event. On the langchain fixture that is 16
   * of 42 edges converging on a node captioned `211708`, dominating the layout
   * while saying nothing about who did what.
   *
   * It is also redundant here: the task *is* the grouping, and one task can span
   * several traces. The server's own `graph_query::STRUCTURAL_RELATION_TYPES`
   * already excludes `PART_OF` from traversals for the same reason; this keeps
   * the task graph consistent with that.
   *
   * Filtered at the presentation layer, not in the API — `/tasks/:id/graph`
   * still returns every edge, because an auditor may legitimately want the trace
   * grouping.
   */
  const STRUCTURAL_RELATIONS: RelationType[] = ['PART_OF']

  const taskGraphRelations = computed<RelationEdge[]>(() =>
    taskGraph.value
      ? taskGraph.value.relations.filter(
          r => !STRUCTURAL_RELATIONS.includes(r.relation_type),
        )
      : [],
  )

  /** How many edges the filter above hid, so the view can say so. */
  const taskGraphStructuralCount = computed(() =>
    taskGraph.value
      ? taskGraph.value.relations.length - taskGraphRelations.value.length
      : 0,
  )

  async function fetchTasks(params: {
    target_id?: string; real_boundaries_only?: boolean; status?: TaskStatus;
    limit?: number; page?: number
  }) {
    try {
      taskLoading.value = true; taskError.value = null
      const res = await client.getTasks(params)
      taskList.value  = res.records
      taskTotal.value = res.total
    } catch (e: any) {
      taskError.value = e.message ?? 'Failed to fetch tasks'
      taskList.value  = []
      taskTotal.value = 0
    } finally {
      taskLoading.value = false
    }
  }

  async function fetchActors(params: {
    kind?: ActorKind | ''; task_id?: string; limit?: number; page?: number
  }) {
    try {
      taskLoading.value = true; taskError.value = null
      const res = await client.getActors(params)
      actorList.value  = res.records
      actorTotal.value = res.total
    } catch (e: any) {
      taskError.value = e.message ?? 'Failed to fetch actors'
      actorList.value  = []
      actorTotal.value = 0
    } finally {
      taskLoading.value = false
    }
  }

  /** Select a task and load its interaction graph — the audit payload. */
  async function selectTask(task: TaskRecord | null) {
    selectedTask.value = task
    taskGraph.value = null
    similarTasks.value = []
    searchError.value = null
    searchUnavailable.value = false
    if (!task) return

    try {
      taskLoading.value = true; taskError.value = null
      taskGraph.value = await client.getTaskGraph(task.task_id)
    } catch (e: any) {
      taskError.value = e.response?.data?.error ?? e.message ?? 'Failed to load the task graph'
    } finally {
      taskLoading.value = false
    }
  }

  /**
   * Find tasks whose stated intent is semantically close to this one's.
   *
   * A task with no `task_embeddings` row yields a 400 from the backend. That is
   * an expected state, not an error: intents are only embedded when an
   * embedding provider is configured, and a task whose log never stated a goal
   * has no intent to embed. Reported through `searchUnavailable` so the view can
   * explain it rather than showing a raw failure.
   */
  async function findSimilarTasks(taskId: string, limit = 10) {
    try {
      searching.value = true
      searchError.value = null
      searchUnavailable.value = false
      similarTasks.value = []
      const res = await client.searchEmbeddings({
        task_id: taskId, kind: 'task', limit, include_self: false,
      })
      similarTasks.value = res.hits
    } catch (e: any) {
      const detail: string | undefined = e.response?.data?.error
      if (e.response?.status === 400) {
        searchUnavailable.value = true
        searchError.value = detail ?? 'This task has no intent embedding yet.'
      } else {
        searchError.value = detail ?? e.message ?? 'Search failed'
      }
    } finally {
      searching.value = false
    }
  }

  function clearTaskError() {
    taskError.value = null
    searchError.value = null
  }

  return {
    // state
    metadataList, metadataTotal, selected, loading, error,
    filterTargetId, filterEntityType, filterRelationType,
    // server-fetched UpsideGate output (Phase 6)
    serverRelations, serverProvTriples, serverSpans, serverDataAvailable,
    // computed (client-side fallbacks)
    entities, relations, provTriples, otelSpans,
    filteredEntities, filteredRelations,
    entityTypeCounts, relationTypeCounts,
    // graph traversal
    expansion, expanding, expansionError, expansionKind,
    graphRelations, graphEntities,
    expandDownstream, expandUpstream, findPath, clearExpansion, resolveEntity,
    // tasks and actors (Stages 11–14)
    taskList, taskTotal, selectedTask, taskGraph, taskLoading, taskError,
    realBoundariesOnly, filterTaskStatus, actorList, actorTotal, filterActorKind,
    similarTasks, searching, searchError, searchUnavailable,
    taskGraphNodes, taskGraphRelations, taskGraphStructuralCount,
    fetchTasks, fetchActors, selectTask, findSimilarTasks, clearTaskError,
    // actions
    fetchMetadata, fetchMetadataByHash, fetchServerOutputs, clearServerOutputs,
    selectMetadata, clearError,
  }
})
