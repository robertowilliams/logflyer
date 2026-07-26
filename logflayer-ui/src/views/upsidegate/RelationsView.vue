<template>
  <div class="space-y-4">
    <!-- Sample selector -->
    <div class="flex flex-wrap gap-3 items-center">
      <input
        v-model="sampleHash"
        @keyup.enter="loadByHash"
        placeholder="Enter sample hash…"
        class="input flex-1 min-w-0 font-mono text-sm"
      />
      <button @click="loadByHash" class="btn-primary">Load</button>
      <button @click="openSamplePicker = !openSamplePicker" class="btn-secondary">Browse samples</button>
    </div>

    <!-- Sample picker dropdown -->
    <div v-if="openSamplePicker" class="card bg-[#0f0f0f] border-[#dc143c]/30">
      <div class="flex gap-2 mb-3">
        <select v-model="targetIdFilter" @change="loadMetadataList" class="input w-44 text-sm">
          <option value="">All targets</option>
          <option v-for="c in store.sampleCollections" :key="c" :value="c">{{ c }}</option>
        </select>
        <span class="text-[rgba(245,245,220,0.40)] text-sm self-center ml-auto">{{ ugStore.metadataTotal }} samples</span>
      </div>
      <div v-if="ugStore.loading" class="text-[rgba(245,245,220,0.40)] text-sm py-4 text-center">Loading…</div>
      <div v-else-if="ugStore.metadataList.length === 0" class="text-[rgba(245,245,220,0.30)] text-sm py-4 text-center">No data yet.</div>
      <div v-else class="space-y-1 max-h-64 overflow-y-auto">
        <button
          v-for="m in ugStore.metadataList"
          :key="m.sample_hash"
          @click="pickSample(m.sample_hash)"
          class="w-full text-left px-3 py-2 rounded hover:bg-[#dc143c]/10 transition-colors flex items-center justify-between gap-4"
        >
          <span class="font-mono text-[#00d4ff] text-xs">{{ m.sample_hash.slice(0, 16) }}…</span>
          <span class="text-[#dc143c] text-xs">{{ m.target_id }}</span>
          <span class="badge-slate ml-auto">{{ m.relation_count }} rel</span>
          <span class="text-[rgba(245,245,220,0.30)] text-[10px]">{{ fmt(m.analyzed_at) }}</span>
        </button>
      </div>
    </div>

    <!-- Error banner -->
    <div v-if="ugStore.error" class="card border-[#dc143c]/60 bg-[#dc143c]/10 text-[#ff6b8a] text-sm flex items-center justify-between">
      <span>{{ ugStore.error }}</span>
      <button @click="ugStore.clearError()" class="hover:text-[#f5f5dc]"><X :size="14" /></button>
    </div>

    <!-- No selection state -->
    <div v-if="!ugStore.selected && !ugStore.loading" class="card text-center py-12 text-[rgba(245,245,220,0.30)]">
      <div class="flex justify-center mb-3"><Share2 :size="36" class="text-[rgba(245,245,220,0.30)]" /></div>
      <div class="text-sm">Select a sample above to explore its relation graph.</div>
    </div>

    <!-- Loading -->
    <div v-else-if="ugStore.loading && !ugStore.selected" class="card text-center py-8 text-[rgba(245,245,220,0.40)] text-sm">
      Loading relations…
    </div>

    <!-- Relations content -->
    <template v-if="ugStore.selected">
      <!-- Header -->
      <div class="card">
        <div class="flex items-center justify-between mb-3">
          <div>
            <h2 class="text-sm font-semibold text-[#f5f5dc]">Relation Graph</h2>
            <div class="text-xs text-[rgba(245,245,220,0.40)] font-mono mt-0.5">
              {{ ugStore.selected.sample_hash }} · {{ ugStore.selected.target_id }}
            </div>
          </div>
          <div class="flex items-center gap-3">
            <!-- Relation type filter -->
            <select v-model="ugStore.filterRelationType" class="input text-xs w-36">
              <option value="">All types</option>
              <option v-for="rt in RELATION_TYPES" :key="rt" :value="rt">{{ rt }}</option>
            </select>
            <button @click="ugStore.selectMetadata(null); sampleHash = ''" class="text-[rgba(245,245,220,0.40)] hover:text-[#f5f5dc] text-xs inline-flex items-center gap-1"><X :size="13" />Clear</button>
          </div>
        </div>

        <!-- Relation type pills -->
        <div class="flex flex-wrap gap-2">
          <button
            v-for="(count, rt) in ugStore.relationTypeCounts"
            :key="rt"
            @click="ugStore.filterRelationType = ugStore.filterRelationType === rt ? '' : rt as any"
            :class="[
              'px-3 py-1 rounded-full text-xs font-mono border transition-all',
              ugStore.filterRelationType === rt
                ? 'bg-[#dc143c]/20 border-[#dc143c]/60 text-[#f5f5dc]'
                : 'bg-[#1a1a1a] border-[#333] text-[rgba(245,245,220,0.60)] hover:border-[#dc143c]/40',
            ]"
          >
            {{ rt }} <span class="ml-1 opacity-70">{{ count }}</span>
          </button>
        </div>
      </div>

      <!-- Traversal banner — shown while a server-side expansion overrides the
           sample-scoped view. -->
      <div
        v-if="ugStore.expansion"
        class="card border-[#00d4ff]/40 bg-[#00d4ff]/5 text-sm flex items-center justify-between gap-4"
      >
        <div class="flex items-center gap-2 min-w-0">
          <component
            :is="ugStore.expansionKind === 'path' ? Route : Waypoints"
            :size="16"
            class="text-[#00d4ff] shrink-0"
          />
          <span v-if="ugStore.expansionKind === 'path'" class="text-[rgba(245,245,220,0.70)] truncate">
            Showing <span class="text-[#00d4ff]">shortest path</span> from
            <code class="text-xs">{{ ugStore.expansion.root.slice(0, 12) }}</code>
            — {{ ugStore.expansion.depth_reached }}
            {{ ugStore.expansion.depth_reached === 1 ? 'hop' : 'hops' }},
            {{ ugStore.expansion.node_count }} entities
          </span>
          <span v-else class="text-[rgba(245,245,220,0.70)] truncate">
            Showing
            <span class="text-[#00d4ff]">{{ ugStore.expansion.direction }}</span>
            traversal from
            <code class="text-xs">{{ ugStore.expansion.root.slice(0, 12) }}</code>
            — {{ ugStore.expansion.node_count }} entities,
            {{ ugStore.expansion.edge_count }} edges,
            depth {{ ugStore.expansion.depth_reached }}
          </span>
        </div>
        <div class="flex items-center gap-3 shrink-0">
          <span v-if="ugStore.expansion.truncated" class="text-[#f59e0b] text-xs">
            truncated at the node limit
          </span>
          <button
            @click="ugStore.clearExpansion()"
            class="text-[rgba(245,245,220,0.50)] hover:text-[#f5f5dc] inline-flex items-center gap-1 text-xs"
          >
            <X :size="13" />Back to sample
          </button>
        </div>
      </div>

      <div v-if="ugStore.expansionError" class="card border-[#f59e0b]/50 bg-[#f59e0b]/10 text-[#f59e0b] text-sm flex items-center justify-between">
        <span>{{ ugStore.expansionError }}</span>
        <button @click="ugStore.clearExpansion()" class="hover:text-[#f5f5dc]"><X :size="14" /></button>
      </div>

      <!-- Force-directed relation graph -->
      <div v-if="ugStore.graphRelations.length > 0" class="card">
        <div class="text-xs text-[rgba(245,245,220,0.40)] mb-3 flex items-center justify-between">
          <span>Drag · scroll to zoom · double-click node or edge for data · triple-click to pin</span>
          <span class="flex items-center gap-2">
            <span v-if="ugStore.expanding" class="text-[#00d4ff]">traversing…</span>
            <span>{{ uniqueEntityIds.length }} entities · {{ ugStore.graphRelations.length }} relations</span>
          </span>
        </div>
        <RelationGraph
          :relations="ugStore.graphRelations"
          :entities="ugStore.graphEntities"
          @expand="showGraphModal = true"
          @detach="openDetached"
          @traverse-downstream="id => ugStore.expandDownstream(id, traversalDepth)"
          @traverse-upstream="id => ugStore.expandUpstream(id, traversalDepth)"
          @find-path="(from, to) => ugStore.findPath(from, to, pathMaxDepth)"
        />
      </div>

      <!-- A traversal that found no edges — e.g. expanding a leaf downstream.
           Distinct from "this sample has no relations", so say which it is. -->
      <div
        v-else-if="ugStore.expansion"
        class="card text-center py-10 text-sm text-[rgba(245,245,220,0.40)]"
      >
        <div class="flex justify-center mb-3"><Waypoints :size="28" class="text-[rgba(245,245,220,0.25)]" /></div>
        No {{ ugStore.expansion.direction }} edges from
        <code class="text-xs">{{ ugStore.expansion.root.slice(0, 12) }}</code> —
        it is a {{ ugStore.expansion.direction === 'downstream' ? 'leaf' : 'root' }} in the graph.
      </div>

      <!-- Expanded graph pop-up -->
      <Teleport to="body">
        <Transition name="rg-fade">
          <div
            v-if="showGraphModal"
            class="fixed inset-0 z-50 bg-black/70 flex items-center justify-center p-6"
            @click.self="showGraphModal = false"
          >
            <div class="card bg-[#0f0f0f] border-[#dc143c]/30 w-full max-w-6xl h-[85vh] flex flex-col p-4">
              <div class="flex items-center justify-between mb-3">
                <div>
                  <h2 class="text-sm font-semibold text-[#f5f5dc]">Relation Graph</h2>
                  <div class="text-xs text-[rgba(245,245,220,0.40)] mt-0.5">
                    {{ uniqueEntityIds.length }} entities · {{ ugStore.graphRelations.length }} relations —
                    scroll to zoom, drag nodes, drag canvas to pan, triple-click to pin
                  </div>
                </div>
                <button
                  @click="showGraphModal = false"
                  class="text-[rgba(245,245,220,0.50)] hover:text-[#f5f5dc] inline-flex items-center gap-1 text-sm"
                >
                  <X :size="16" />Close
                </button>
              </div>
              <div class="flex-1 min-h-0">
                <RelationGraph
                  :relations="ugStore.graphRelations"
                  :entities="ugStore.graphEntities"
                  expanded
                  @traverse-downstream="id => ugStore.expandDownstream(id, traversalDepth)"
                  @traverse-upstream="id => ugStore.expandUpstream(id, traversalDepth)"
                  @find-path="(from, to) => ugStore.findPath(from, to, pathMaxDepth)"
                />
              </div>
            </div>
          </div>
        </Transition>
      </Teleport>

      <!-- Relations table -->
      <div class="card p-0 overflow-hidden">
        <div class="bg-[#0f0f0f] px-4 py-2 text-xs text-[rgba(245,245,220,0.40)] font-semibold uppercase tracking-wider border-b border-[#dc143c]/20">
          Relation edges ({{ ugStore.filteredRelations.length }})
        </div>
        <div v-if="ugStore.filteredRelations.length === 0" class="px-4 py-6 text-center text-[rgba(245,245,220,0.30)] text-sm">
          No relations match the current filter.
        </div>
        <table v-else class="w-full text-xs">
          <thead class="bg-[#0f0f0f]">
            <tr class="text-[rgba(245,245,220,0.50)] text-left border-b border-[#dc143c]/20">
              <th class="px-3 py-2">Source Entity</th>
              <th class="px-3 py-2 text-center"><span class="inline-flex items-center gap-1"><ArrowRight :size="12" />Type</span></th>
              <th class="px-3 py-2">Target Entity</th>
              <th class="px-3 py-2 text-right">Confidence</th>
              <th class="px-3 py-2">Relation ID</th>
            </tr>
          </thead>
          <tbody class="divide-y divide-[#1a1a1a]">
            <tr v-for="r in ugStore.filteredRelations" :key="r.relation_id" class="hover:bg-[#dc143c]/5 transition-colors">
              <td class="px-3 py-2 font-mono text-[#00d4ff]">{{ entityLabel(r.source_entity_id) }}</td>
              <td class="px-3 py-2 text-center">
                <span :class="relationTypeClass(r.relation_type)">{{ r.relation_type }}</span>
              </td>
              <td class="px-3 py-2 font-mono text-[#00d4ff]">{{ entityLabel(r.target_entity_id) }}</td>
              <td class="px-3 py-2 text-right" :class="r.confidence >= 0.8 ? 'text-green-400' : 'text-yellow-400'">
                {{ (r.confidence * 100).toFixed(0) }}%
              </td>
              <td class="px-3 py-2 font-mono text-[rgba(245,245,220,0.30)] text-[10px]">{{ r.relation_id.slice(0, 16) }}…</td>
            </tr>
          </tbody>
        </table>
      </div>
    </template>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { useLogflayerStore } from '../../stores/logflayer'
import { useUpsidegateStore } from '../../stores/upsidegate'
import type { RelationType } from '../../types'
import { X, Share2, ArrowRight, Waypoints, Route } from 'lucide-vue-next'
import RelationGraph from '../../components/RelationGraph.vue'

const store   = useLogflayerStore()
const ugStore = useUpsidegateStore()
const router  = useRouter()

function openDetached() {
  const hash = ugStore.selected?.sample_hash
  if (!hash) return
  const href = router.resolve({ name: 'detached-graph', query: { hash } }).href
  window.open(href, `vecta-graph-${hash}`, 'width=1280,height=860,noopener')
}

const sampleHash      = ref('')
const openSamplePicker = ref(false)
const targetIdFilter  = ref('')
const showGraphModal  = ref(false)
/** Hops requested when expanding a node. Two is enough to show a node's
 *  neighbourhood without pulling in most of the graph. */
const traversalDepth  = 2
/** Hops to search when resolving a path. Deliberately larger than
 *  `traversalDepth`: a path search is looking for a specific target and only
 *  returns the winning chain, so a wider search costs the user nothing. */
const pathMaxDepth    = 6

const RELATION_TYPES: RelationType[] = [
  'TRIGGERED_BY', 'GENERATED', 'INFORMED', 'FOLLOWED_BY',
  'RESPONDED_TO', 'ASSEMBLED_FROM', 'PART_OF', 'DELEGATED_TO',
]

// Counts whatever the graph is currently drawing — the traversal when one is
// active, otherwise the sample's own relations.
const uniqueEntityIds = computed(() => {
  const ids = new Set<string>()
  for (const r of ugStore.graphRelations) {
    ids.add(r.source_entity_id)
    ids.add(r.target_entity_id)
  }
  return [...ids]
})

function entityLabel(eid: string): string {
  const e = ugStore.graphEntities.find(x => x.entity_id === eid)
  if (!e) return eid.slice(0, 12) + '…'
  const base = e.tool_name ?? e.entity_type
  return `${base} (${eid.slice(0, 6)})`
}

function fmt(ts: string) {
  try { return new Date(ts).toLocaleString() } catch { return ts }
}

function relationTypeClass(rt: string) {
  const map: Record<string, string> = {
    TRIGGERED_BY:   'badge-red',
    GENERATED:      'badge-green',
    INFORMED:       'badge-blue',
    FOLLOWED_BY:    'badge-slate',
    RESPONDED_TO:   'badge-green',
    ASSEMBLED_FROM: 'badge-yellow',
    PART_OF:        'badge-yellow',
    DELEGATED_TO:   'badge-blue',
  }
  return map[rt] ?? 'badge-slate'
}

async function loadByHash() {
  if (!sampleHash.value.trim()) return
  openSamplePicker.value = false
  await ugStore.fetchMetadataByHash(sampleHash.value.trim())
}

function pickSample(hash: string) {
  sampleHash.value = hash
  openSamplePicker.value = false
  ugStore.fetchMetadataByHash(hash)
}

async function loadMetadataList() {
  await ugStore.fetchMetadata({ target_id: targetIdFilter.value || undefined, limit: 20, page: 1 })
}

onMounted(async () => {
  await store.fetchSampleCollections()
  await ugStore.fetchMetadata({ limit: 20, page: 1 })
})
</script>

<style scoped>
.rg-fade-enter-active,
.rg-fade-leave-active {
  transition: opacity 0.15s ease;
}
.rg-fade-enter-from,
.rg-fade-leave-to {
  opacity: 0;
}
</style>
