<script setup lang="ts">
/**
 * Detached, chrome-less relation-graph window.
 * Opened via the graph's detach button (window.open with ?hash=…). Runs in its
 * own browser window / Pinia instance, so it re-fetches the sample by hash and
 * renders the force graph full-window for side-by-side visual analysis.
 */
import { computed, onMounted } from 'vue'
import { useRoute } from 'vue-router'
import { useUpsidegateStore } from '../../stores/upsidegate'
import type { RelationType } from '../../types'
import { Waypoints, X } from 'lucide-vue-next'
import RelationGraph from '../../components/RelationGraph.vue'
import circlesLogoUrl from '../../assets/circles-logo.svg'

const route = useRoute()
const ugStore = useUpsidegateStore()

const hash = computed(() => (route.query.hash as string) || '')

const RELATION_TYPES: RelationType[] = [
  'TRIGGERED_BY', 'GENERATED', 'INFORMED', 'FOLLOWED_BY',
  'RESPONDED_TO', 'ASSEMBLED_FROM', 'PART_OF', 'DELEGATED_TO',
]

// Reflects whatever the graph is drawing — the traversal overlay when one is
// active, otherwise the sample's own relations.
const entityCount = computed(() => {
  const ids = new Set<string>()
  for (const r of ugStore.graphRelations) {
    ids.add(r.source_entity_id)
    ids.add(r.target_entity_id)
  }
  return ids.size
})

/** Hops requested when expanding a node from this window. */
const traversalDepth = 2

onMounted(async () => {
  if (hash.value) await ugStore.fetchMetadataByHash(hash.value)
})
</script>

<template>
  <div class="h-screen w-screen bg-[#0a0a0a] text-[#f5f5dc] flex flex-col overflow-hidden">
    <!-- Compact header -->
    <header class="shrink-0 flex items-center gap-4 px-4 py-2.5 border-b border-[#dc143c]/20 bg-[#0f0f0f]">
      <img :src="circlesLogoUrl" alt="VectaDB" class="h-4 w-auto" />
      <div class="min-w-0">
        <div class="text-sm font-semibold leading-tight">Relation Graph</div>
        <div class="text-[10px] text-[rgba(245,245,220,0.40)] font-mono truncate">{{ hash }}</div>
      </div>
      <div class="ml-auto flex items-center gap-3">
        <button
          v-if="ugStore.expansion"
          class="inline-flex items-center gap-1 px-2 py-1 rounded text-[11px] border
                 border-[#00d4ff]/40 bg-[#00d4ff]/10 text-[#00d4ff] hover:bg-[#00d4ff]/20 transition-colors"
          :title="`Showing ${ugStore.expansion.direction} traversal — click to return to the sample`"
          @click="ugStore.clearExpansion()"
        >
          <Waypoints :size="12" />{{ ugStore.expansion.direction }} · depth {{ ugStore.expansion.depth_reached }}
          <X :size="12" />
        </button>
        <span v-if="ugStore.selected" class="text-xs text-[rgba(245,245,220,0.45)]">
          {{ entityCount }} entities · {{ ugStore.graphRelations.length }} relations
        </span>
        <select v-model="ugStore.filterRelationType" class="input text-xs w-36">
          <option value="">All types</option>
          <option v-for="rt in RELATION_TYPES" :key="rt" :value="rt">{{ rt }}</option>
        </select>
      </div>
    </header>

    <!-- Graph -->
    <main class="flex-1 min-h-0 p-3">
      <div v-if="ugStore.loading" class="h-full flex items-center justify-center text-[rgba(245,245,220,0.40)] text-sm">
        Loading relations…
      </div>
      <div
        v-else-if="!ugStore.selected"
        class="h-full flex items-center justify-center text-[rgba(245,245,220,0.30)] text-sm"
      >
        Could not load sample <span class="font-mono ml-1">{{ hash || '(no hash)' }}</span>
      </div>
      <div
        v-else-if="ugStore.graphRelations.length === 0"
        class="h-full flex items-center justify-center text-[rgba(245,245,220,0.30)] text-sm"
      >
        <span v-if="ugStore.expansion">
          No {{ ugStore.expansion.direction }} edges from
          <code class="text-xs">{{ ugStore.expansion.root.slice(0, 12) }}</code>.
        </span>
        <span v-else>No relations for this sample / filter.</span>
      </div>
      <RelationGraph
        v-else
        :relations="ugStore.graphRelations"
        :entities="ugStore.graphEntities"
        expanded
        @traverse-downstream="id => ugStore.expandDownstream(id, traversalDepth)"
        @traverse-upstream="id => ugStore.expandUpstream(id, traversalDepth)"
      />
    </main>
  </div>
</template>
