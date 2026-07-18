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
      <button @click="ugStore.clearError()" class="hover:text-[#f5f5dc]">✕</button>
    </div>

    <!-- No selection state -->
    <div v-if="!ugStore.selected && !ugStore.loading" class="card text-center py-12 text-[rgba(245,245,220,0.30)]">
      <div class="text-4xl mb-3">🔗</div>
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
            <button @click="ugStore.selectMetadata(null); sampleHash = ''" class="text-[rgba(245,245,220,0.40)] hover:text-[#f5f5dc] text-xs">✕ Clear</button>
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

      <!-- SVG mini-graph (entity nodes + arrows) -->
      <div v-if="ugStore.filteredRelations.length > 0" class="card overflow-x-auto">
        <div class="text-xs text-[rgba(245,245,220,0.40)] mb-3">Visual overview (simplified force layout)</div>
        <svg
          :width="Math.max(600, uniqueEntityIds.length * 120)"
          height="200"
          class="block"
        >
          <defs>
            <marker id="arrow" markerWidth="6" markerHeight="6" refX="5" refY="3" orient="auto">
              <path d="M0,0 L0,6 L6,3 z" fill="rgba(220,20,60,0.7)" />
            </marker>
          </defs>
          <!-- Entity nodes -->
          <g v-for="(eid, idx) in uniqueEntityIds" :key="eid">
            <circle
              :cx="nodeX(idx)"
              cy="100"
              r="24"
              fill="#1a1a1a"
              stroke="#dc143c"
              stroke-opacity="0.4"
              stroke-width="1"
            />
            <text
              :x="nodeX(idx)"
              y="104"
              text-anchor="middle"
              font-size="8"
              fill="rgba(245,245,220,0.6)"
              class="select-none"
            >{{ eid.slice(0, 8) }}</text>
          </g>
          <!-- Relation edges -->
          <g v-for="r in ugStore.filteredRelations" :key="r.relation_id">
            <line
              :x1="nodeX(uniqueEntityIds.indexOf(r.source_entity_id)) + 24"
              y1="100"
              :x2="Math.max(nodeX(uniqueEntityIds.indexOf(r.target_entity_id)) - 24, nodeX(uniqueEntityIds.indexOf(r.source_entity_id)) + 28)"
              y2="100"
              stroke="rgba(220,20,60,0.5)"
              stroke-width="1.5"
              marker-end="url(#arrow)"
            />
            <text
              :x="(nodeX(uniqueEntityIds.indexOf(r.source_entity_id)) + nodeX(uniqueEntityIds.indexOf(r.target_entity_id))) / 2"
              y="92"
              text-anchor="middle"
              font-size="7"
              fill="rgba(0,212,255,0.7)"
              class="select-none"
            >{{ r.relation_type }}</text>
          </g>
        </svg>
      </div>

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
              <th class="px-3 py-2 text-center">→ Type</th>
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
import { useLogflayerStore } from '../../stores/logflayer'
import { useUpsidegateStore } from '../../stores/upsidegate'
import type { RelationType } from '../../types'

const store   = useLogflayerStore()
const ugStore = useUpsidegateStore()

const sampleHash      = ref('')
const openSamplePicker = ref(false)
const targetIdFilter  = ref('')

const RELATION_TYPES: RelationType[] = [
  'TRIGGERED_BY', 'GENERATED', 'INFORMED', 'FOLLOWED_BY',
  'RESPONDED_TO', 'ASSEMBLED_FROM', 'PART_OF', 'DELEGATED_TO',
]

const uniqueEntityIds = computed(() => {
  const ids = new Set<string>()
  for (const r of ugStore.filteredRelations) {
    ids.add(r.source_entity_id)
    ids.add(r.target_entity_id)
  }
  return [...ids]
})

function nodeX(idx: number) {
  const total = uniqueEntityIds.value.length
  const gap   = Math.max(600, total * 120) / Math.max(total, 1)
  return gap * idx + gap / 2
}

function entityLabel(eid: string): string {
  const e = ugStore.entities.find(x => x.entity_id === eid)
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
