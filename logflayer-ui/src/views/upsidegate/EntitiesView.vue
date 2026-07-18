<template>
  <div class="space-y-4">
    <!-- Sample selector + filters -->
    <div class="flex flex-wrap gap-3 items-center">
      <select v-model="targetId" @change="loadPage(1)" class="input w-44">
        <option value="">All targets</option>
        <option v-for="c in store.sampleCollections" :key="c" :value="c">{{ c }}</option>
      </select>

      <select v-model="ugStore.filterEntityType" class="input w-44">
        <option value="">All entity types</option>
        <option v-for="et in ENTITY_TYPES" :key="et" :value="et">{{ et }}</option>
      </select>

      <button @click="loadPage(1)" class="btn-primary">↻ Refresh</button>

      <span class="ml-auto text-[rgba(245,245,220,0.40)] text-sm self-center">
        {{ ugStore.metadataTotal }} sample(s) ·
        {{ ugStore.entities.length }} entities in view
      </span>
    </div>

    <!-- Metadata sample list -->
    <div class="card p-0 overflow-hidden">
      <div class="bg-[#0f0f0f] px-4 py-2 text-xs text-[rgba(245,245,220,0.40)] font-semibold uppercase tracking-wider border-b border-[#dc143c]/20">
        Sample index — click to inspect entities
      </div>
      <div v-if="ugStore.loading" class="px-4 py-6 text-center text-[rgba(245,245,220,0.40)] text-sm">
        Loading…
      </div>
      <div v-else-if="ugStore.metadataList.length === 0" class="px-4 py-8 text-center text-[rgba(245,245,220,0.30)] text-sm">
        No preprocessed metadata found.
        <span class="block mt-1 text-[rgba(245,245,220,0.20)] text-xs">
          The /api/v1/metadata endpoint will be available once Phase 8 is deployed.
        </span>
      </div>
      <table v-else class="w-full text-sm">
        <thead class="bg-[#0f0f0f]">
          <tr class="text-[rgba(245,245,220,0.50)] text-left border-b border-[#dc143c]/20">
            <th class="px-4 py-3">Sample Hash</th>
            <th class="px-4 py-3">Target</th>
            <th class="px-4 py-3">Analyzed At</th>
            <th class="px-4 py-3">Format</th>
            <th class="px-4 py-3 text-right">Entities</th>
            <th class="px-4 py-3 text-right">Relations</th>
            <th class="px-4 py-3">Status</th>
          </tr>
        </thead>
        <tbody class="divide-y divide-[#1a1a1a]">
          <tr
            v-for="m in ugStore.metadataList"
            :key="m.sample_hash"
            class="hover:bg-[#dc143c]/5 transition-colors cursor-pointer"
            :class="ugStore.selected?.sample_hash === m.sample_hash ? 'bg-[#dc143c]/10 border-l-2 border-[#dc143c]' : ''"
            @click="ugStore.selectMetadata(ugStore.selected?.sample_hash === m.sample_hash ? null : m)"
          >
            <td class="px-4 py-2 font-mono text-[#00d4ff] text-xs">{{ m.sample_hash.slice(0, 12) }}…</td>
            <td class="px-4 py-2 text-[#dc143c] text-xs font-mono">{{ m.target_id }}</td>
            <td class="px-4 py-2 text-[rgba(245,245,220,0.40)] text-xs">{{ fmt(m.analyzed_at) }}</td>
            <td class="px-4 py-2"><span class="badge-blue">{{ m.format.log_type }}</span></td>
            <td class="px-4 py-2 text-right text-[rgba(245,245,220,0.70)]">{{ m.entity_count }}</td>
            <td class="px-4 py-2 text-right text-[rgba(245,245,220,0.70)]">{{ m.relation_count }}</td>
            <td class="px-4 py-2">
              <span :class="statusClass(m.classification_status)">{{ m.classification_status }}</span>
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <!-- Pagination -->
    <div class="flex items-center justify-between text-sm text-[rgba(245,245,220,0.40)]">
      <span>Page {{ page }}</span>
      <div class="flex gap-2">
        <button :disabled="page <= 1" @click="loadPage(page - 1)" class="btn-secondary py-1 disabled:opacity-40">← Prev</button>
        <button :disabled="page * limit >= ugStore.metadataTotal" @click="loadPage(page + 1)" class="btn-secondary py-1 disabled:opacity-40">Next →</button>
      </div>
    </div>

    <!-- Entity browser panel (shown when a sample is selected) -->
    <div v-if="ugStore.selected" class="space-y-3">
      <!-- Type summary pills -->
      <div class="card">
        <div class="flex items-center justify-between mb-3">
          <h2 class="text-sm font-semibold text-[#f5f5dc]">
            Entity breakdown — <span class="text-[#00d4ff] font-mono text-xs">{{ ugStore.selected.sample_hash.slice(0, 16) }}…</span>
          </h2>
          <button @click="ugStore.selectMetadata(null)" class="text-[rgba(245,245,220,0.40)] hover:text-[#f5f5dc] transition-colors text-xs">✕ Close</button>
        </div>
        <div class="flex flex-wrap gap-2">
          <button
            v-for="(count, et) in ugStore.entityTypeCounts"
            :key="et"
            @click="ugStore.filterEntityType = ugStore.filterEntityType === et ? '' : et as any"
            :class="[
              'px-3 py-1 rounded-full text-xs font-mono border transition-all',
              ugStore.filterEntityType === et
                ? 'bg-[#dc143c]/20 border-[#dc143c]/60 text-[#f5f5dc]'
                : 'bg-[#1a1a1a] border-[#333] text-[rgba(245,245,220,0.60)] hover:border-[#dc143c]/40',
            ]"
          >
            {{ et }} <span class="ml-1 opacity-70">{{ count }}</span>
          </button>
          <button
            v-if="ugStore.filterEntityType"
            @click="ugStore.filterEntityType = ''"
            class="px-3 py-1 rounded-full text-xs border border-dashed border-[rgba(245,245,220,0.20)] text-[rgba(245,245,220,0.40)] hover:text-[#f5f5dc] transition-all"
          >
            Clear filter
          </button>
        </div>
      </div>

      <!-- Entity table -->
      <div class="card p-0 overflow-hidden">
        <div class="bg-[#0f0f0f] px-4 py-2 text-xs text-[rgba(245,245,220,0.40)] font-semibold uppercase tracking-wider border-b border-[#dc143c]/20">
          Entities ({{ ugStore.filteredEntities.length }})
        </div>
        <div v-if="ugStore.filteredEntities.length === 0" class="px-4 py-6 text-center text-[rgba(245,245,220,0.30)] text-sm">
          No entities match the current filter.
        </div>
        <table v-else class="w-full text-xs">
          <thead class="bg-[#0f0f0f]">
            <tr class="text-[rgba(245,245,220,0.50)] text-left border-b border-[#dc143c]/20">
              <th class="px-3 py-2">Type</th>
              <th class="px-3 py-2">Raw value</th>
              <th class="px-3 py-2">Semantic role</th>
              <th class="px-3 py-2">Tool / MCP</th>
              <th class="px-3 py-2 text-right">Line</th>
              <th class="px-3 py-2 text-right">Latency</th>
              <th class="px-3 py-2">Span ID</th>
            </tr>
          </thead>
          <tbody class="divide-y divide-[#1a1a1a]">
            <tr
              v-for="e in ugStore.filteredEntities"
              :key="e.entity_id"
              class="hover:bg-[#dc143c]/5 transition-colors"
              @click="selectedEntity = selectedEntity?.entity_id === e.entity_id ? null : e"
              :class="selectedEntity?.entity_id === e.entity_id ? 'bg-[#dc143c]/8' : ''"
            >
              <td class="px-3 py-2">
                <span :class="entityTypeClass(e.entity_type)">{{ e.entity_type }}</span>
              </td>
              <td class="px-3 py-2 font-mono text-[rgba(245,245,220,0.80)] max-w-xs truncate" :title="e.raw_text">
                {{ e.raw_text.length > 60 ? e.raw_text.slice(0, 60) + '…' : e.raw_text }}
              </td>
              <td class="px-3 py-2 text-[rgba(245,245,220,0.50)]">{{ e.semantic_role }}</td>
              <td class="px-3 py-2 text-[rgba(245,245,220,0.50)] font-mono">
                {{ e.tool_name ?? '' }}
                <span v-if="e.mcp_server_id" class="text-[#00d4ff]">@{{ e.mcp_server_id }}</span>
              </td>
              <td class="px-3 py-2 text-right text-[rgba(245,245,220,0.40)]">{{ e.line_index }}</td>
              <td class="px-3 py-2 text-right text-[rgba(245,245,220,0.50)] font-mono text-[10px]">
                {{ e.tool_name ? '' : (e.latency_ms ?? '') }}{{ e.latency_ms ? 'ms' : '' }}
              </td>
              <td class="px-3 py-2 font-mono text-[rgba(245,245,220,0.30)] text-[10px]">
                {{ e.span_id.slice(0, 8) }}…
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <!-- Entity detail drawer -->
      <div v-if="selectedEntity" class="card bg-[#0a0a0a] border-[#00d4ff]/20 text-xs">
        <div class="flex justify-between items-center mb-3">
          <span class="text-[rgba(245,245,220,0.80)] font-semibold">Entity Detail</span>
          <button @click="selectedEntity = null" class="text-[rgba(245,245,220,0.40)] hover:text-[#f5f5dc]">✕</button>
        </div>
        <div class="grid grid-cols-2 gap-x-8 gap-y-2 font-mono">
          <div><span class="text-[rgba(245,245,220,0.40)]">entity_id</span><br><span class="text-[#00d4ff]">{{ selectedEntity.entity_id }}</span></div>
          <div><span class="text-[rgba(245,245,220,0.40)]">entity_type</span><br><span class="text-[#f5f5dc]">{{ selectedEntity.entity_type }}</span></div>
          <div><span class="text-[rgba(245,245,220,0.40)]">semantic_role</span><br><span class="text-[#f5f5dc]">{{ selectedEntity.semantic_role }}</span></div>
          <div v-if="selectedEntity.model_id"><span class="text-[rgba(245,245,220,0.40)]">model_id</span><br><span class="text-[#f5f5dc]">{{ selectedEntity.model_id }}</span></div>
          <div><span class="text-[rgba(245,245,220,0.40)]">trace_id</span><br><span class="text-[rgba(245,245,220,0.50)]">{{ selectedEntity.trace_id }}</span></div>
          <div><span class="text-[rgba(245,245,220,0.40)]">span_id</span><br><span class="text-[rgba(245,245,220,0.50)]">{{ selectedEntity.span_id }}</span></div>
          <div v-if="selectedEntity.parent_span_id"><span class="text-[rgba(245,245,220,0.40)]">parent_span_id</span><br><span class="text-[rgba(245,245,220,0.50)]">{{ selectedEntity.parent_span_id }}</span></div>
          <div v-if="selectedEntity.tool_name"><span class="text-[rgba(245,245,220,0.40)]">tool_name</span><br><span class="text-[#dc143c]">{{ selectedEntity.tool_name }}</span></div>
          <div v-if="selectedEntity.mcp_server_id"><span class="text-[rgba(245,245,220,0.40)]">mcp_server_id</span><br><span class="text-[#00d4ff]">{{ selectedEntity.mcp_server_id }}</span></div>
          <div v-if="selectedEntity.token_count != null"><span class="text-[rgba(245,245,220,0.40)]">token_count</span><br><span class="text-[#f5f5dc]">{{ selectedEntity.token_count }}</span></div>
          <div v-if="selectedEntity.latency_ms != null"><span class="text-[rgba(245,245,220,0.40)]">latency_ms</span><br><span class="text-[#f5f5dc]">{{ selectedEntity.latency_ms }}ms</span></div>
          <div class="col-span-2"><span class="text-[rgba(245,245,220,0.40)]">raw_text</span><br>
            <pre class="mt-1 text-[#00d4ff] whitespace-pre-wrap break-all max-h-32 overflow-auto bg-[#0f0f0f] p-2 rounded">{{ selectedEntity.raw_text }}</pre>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useLogflayerStore } from '../../stores/logflayer'
import { useUpsidegateStore } from '../../stores/upsidegate'
import type { EntityRecord, EntityType } from '../../types'

const store   = useLogflayerStore()
const ugStore = useUpsidegateStore()

const targetId      = ref('')
const page          = ref(1)
const limit         = 50
const selectedEntity = ref<EntityRecord | null>(null)

const ENTITY_TYPES: EntityType[] = [
  'PromptEvent', 'CompletionEvent', 'ToolCallEvent', 'ToolResultEvent',
  'RetrievalEvent', 'AgentStep', 'McpEvent', 'ContextWindow', 'Unknown',
]

function fmt(ts: string) {
  try { return new Date(ts).toLocaleString() } catch { return ts }
}

function statusClass(s: string) {
  // Backend emits snake_case (`pending`, `classified`, `skipped`, `failed`).
  if (s === 'classified') return 'badge-green'
  if (s === 'failed')     return 'badge-red'
  if (s === 'skipped')    return 'badge-yellow'
  return 'badge-slate'
}

function entityTypeClass(et: string) {
  const map: Record<string, string> = {
    PromptEvent:     'badge-blue',
    CompletionEvent: 'badge-green',
    ToolCallEvent:   'badge-blue',
    ToolResultEvent: 'badge-green',
    RetrievalEvent:  'badge-yellow',
    AgentStep:       'badge-yellow',
    McpEvent:        'px-2 py-0.5 rounded text-xs bg-purple-500/20 text-purple-300 border border-purple-500/30',
    ContextWindow:   'badge-slate',
    Unknown:         'badge-slate',
  }
  return map[et] ?? 'badge-slate'
}

async function loadPage(p: number) {
  page.value = p
  selectedEntity.value = null
  await ugStore.fetchMetadata({
    target_id: targetId.value || undefined,
    limit,
    page: p,
  })
}

onMounted(async () => {
  await store.fetchSampleCollections()
  await loadPage(1)
})
</script>
