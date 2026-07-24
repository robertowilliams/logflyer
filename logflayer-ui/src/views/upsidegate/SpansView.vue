<template>
  <div class="space-y-4">
    <!-- Controls -->
    <div class="flex flex-wrap gap-3 items-center">
      <select v-model="targetId" @change="loadPage(1)" class="input w-44">
        <option value="">All targets</option>
        <option v-for="c in store.sampleCollections" :key="c" :value="c">{{ c }}</option>
      </select>
      <button @click="loadPage(1)" class="btn-primary inline-flex items-center gap-1.5"><RefreshCw :size="14" />Refresh</button>
      <span class="ml-auto text-[rgba(245,245,220,0.40)] text-sm self-center">{{ ugStore.metadataTotal }} sample(s)</span>
    </div>

    <!-- Sample list -->
    <div class="card p-0 overflow-hidden">
      <div class="bg-[#0f0f0f] px-4 py-2 text-xs text-[rgba(245,245,220,0.40)] font-semibold uppercase tracking-wider border-b border-[#dc143c]/20">
        Select a sample to view OTel spans
      </div>
      <div v-if="ugStore.loading" class="px-4 py-6 text-center text-[rgba(245,245,220,0.40)] text-sm">Loading…</div>
      <div v-else-if="ugStore.metadataList.length === 0" class="px-4 py-8 text-center text-[rgba(245,245,220,0.30)] text-sm">
        No preprocessed metadata found.
      </div>
      <table v-else class="w-full text-sm">
        <thead class="bg-[#0f0f0f]">
          <tr class="text-[rgba(245,245,220,0.50)] text-left border-b border-[#dc143c]/20">
            <th class="px-4 py-3">Sample Hash</th>
            <th class="px-4 py-3">Target</th>
            <th class="px-4 py-3">Trace ID</th>
            <th class="px-4 py-3 text-right">Entities</th>
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
            <td class="px-4 py-2 font-mono text-[#00d4ff] text-xs">{{ m.sample_hash.slice(0, 16) }}…</td>
            <td class="px-4 py-2 text-[#dc143c] text-xs font-mono">{{ m.target_id }}</td>
            <td class="px-4 py-2 font-mono text-[rgba(245,245,220,0.40)] text-xs">{{ m.otel_trace_id.slice(0, 24) }}…</td>
            <td class="px-4 py-2 text-right text-[rgba(245,245,220,0.70)]">{{ m.entity_count }}</td>
          </tr>
        </tbody>
      </table>
    </div>

    <!-- Pagination -->
    <div class="flex items-center justify-between text-sm text-[rgba(245,245,220,0.40)]">
      <span>Page {{ page }}</span>
      <div class="flex gap-2">
        <button :disabled="page <= 1" @click="loadPage(page - 1)" class="btn-secondary py-1 disabled:opacity-40 inline-flex items-center gap-1"><ChevronLeft :size="14" />Prev</button>
        <button :disabled="page * limit >= ugStore.metadataTotal" @click="loadPage(page + 1)" class="btn-secondary py-1 disabled:opacity-40 inline-flex items-center gap-1">Next<ChevronRight :size="14" /></button>
      </div>
    </div>

    <!-- Spans panel -->
    <div v-if="ugStore.selected" class="space-y-3">
      <!-- Trace header -->
      <div class="card border-[#00d4ff]/20">
        <div class="flex items-center justify-between">
          <div>
            <div class="text-xs text-[rgba(245,245,220,0.40)] mb-1">Root Trace ID</div>
            <div class="font-mono text-[#00d4ff] text-sm">{{ ugStore.selected.otel_trace_id }}</div>
          </div>
          <div class="text-right">
            <div class="text-xs text-[rgba(245,245,220,0.40)] mb-1">Spans (unique)</div>
            <div class="text-2xl font-bold text-[#f5f5dc]">{{ ugStore.otelSpans.length }}</div>
          </div>
          <button @click="ugStore.selectMetadata(null)" class="text-[rgba(245,245,220,0.40)] hover:text-[#f5f5dc] text-xs self-start inline-flex items-center gap-1"><X :size="13" />Clear</button>
        </div>
      </div>

      <!-- Span kind filter -->
      <div class="flex gap-2 flex-wrap">
        <button
          v-for="kind in SPAN_KINDS"
          :key="kind"
          @click="kindFilter = kindFilter === kind ? '' : kind"
          :class="[
            'px-3 py-1 rounded-full text-xs font-mono border transition-all',
            kindFilter === kind
              ? 'bg-[#00d4ff]/20 border-[#00d4ff]/60 text-[#f5f5dc]'
              : 'bg-[#1a1a1a] border-[#333] text-[rgba(245,245,220,0.60)] hover:border-[#00d4ff]/40',
          ]"
        >{{ kind }}</button>
        <button
          v-if="kindFilter"
          @click="kindFilter = ''"
          class="px-3 py-1 rounded-full text-xs border border-dashed border-[rgba(245,245,220,0.20)] text-[rgba(245,245,220,0.40)] hover:text-[#f5f5dc] transition-all"
        >Clear</button>
      </div>

      <!-- Waterfall / timeline (visual) -->
      <div v-if="filteredSpans.length > 0" class="card">
        <div class="text-xs text-[rgba(245,245,220,0.40)] mb-3 font-semibold uppercase tracking-wider">Span timeline (relative, entity order)</div>
        <div class="space-y-2">
          <div
            v-for="(span, idx) in filteredSpans"
            :key="span.span_id"
            class="flex items-center gap-3 group"
          >
            <div class="w-4 text-right text-[10px] text-[rgba(245,245,220,0.30)] font-mono flex-shrink-0">{{ idx + 1 }}</div>
            <div class="flex-shrink-0 w-20 truncate text-[10px] font-mono text-[rgba(245,245,220,0.50)]" :title="span.span_id">
              {{ span.span_id.slice(0, 8) }}…
            </div>
            <!-- Bar -->
            <div class="flex-1 h-6 bg-[#1a1a1a] rounded relative overflow-hidden">
              <div
                class="h-full rounded transition-all"
                :class="spanBarClass(span)"
                :style="`width:${Math.max(4, (idx + 1) / filteredSpans.length * 100)}%`"
              />
              <span class="absolute inset-0 flex items-center px-2 text-[10px] font-mono text-[rgba(245,245,220,0.70)] truncate">
                {{ span.name }}
              </span>
            </div>
            <!-- Kind + status badges -->
            <div class="flex gap-1 flex-shrink-0">
              <span :class="spanKindClass(span.kind)">{{ span.kind }}</span>
              <span :class="statusClass(span.status.code)">{{ span.status.code }}</span>
            </div>
          </div>
        </div>
      </div>

      <!-- Spans table -->
      <div class="card p-0 overflow-hidden">
        <div class="bg-[#0f0f0f] px-4 py-2 text-xs text-[rgba(245,245,220,0.40)] font-semibold uppercase tracking-wider border-b border-[#dc143c]/20">
          OTel Spans ({{ filteredSpans.length }})
        </div>
        <div v-if="filteredSpans.length === 0" class="px-4 py-6 text-center text-[rgba(245,245,220,0.30)] text-sm">
          No spans match the current filter.
        </div>
        <table v-else class="w-full text-xs">
          <thead class="bg-[#0f0f0f]">
            <tr class="text-[rgba(245,245,220,0.50)] text-left border-b border-[#dc143c]/20">
              <th class="px-3 py-2">Span ID</th>
              <th class="px-3 py-2">Name</th>
              <th class="px-3 py-2">Kind</th>
              <th class="px-3 py-2">Status</th>
              <th class="px-3 py-2">Attributes</th>
            </tr>
          </thead>
          <tbody class="divide-y divide-[#1a1a1a]">
            <tr
              v-for="span in filteredSpans"
              :key="span.span_id"
              class="hover:bg-[#dc143c]/5 transition-colors cursor-pointer"
              :class="selectedSpan?.span_id === span.span_id ? 'bg-[#dc143c]/8' : ''"
              @click="selectedSpan = selectedSpan?.span_id === span.span_id ? null : span"
            >
              <td class="px-3 py-2 font-mono text-[rgba(245,245,220,0.50)] text-[10px]">{{ span.span_id.slice(0, 16) }}…</td>
              <td class="px-3 py-2 text-[rgba(245,245,220,0.80)] font-mono">{{ span.name }}</td>
              <td class="px-3 py-2"><span :class="spanKindClass(span.kind)">{{ span.kind }}</span></td>
              <td class="px-3 py-2"><span :class="statusClass(span.status.code)">{{ span.status.code }}</span></td>
              <td class="px-3 py-2 text-[rgba(245,245,220,0.40)]">{{ Object.keys(span.attributes).length }} attr(s)</td>
            </tr>
          </tbody>
        </table>
      </div>

      <!-- Span detail drawer -->
      <div v-if="selectedSpan" class="card bg-[#0a0a0a] border-[#00d4ff]/20 text-xs">
        <div class="flex justify-between items-center mb-3">
          <span class="text-[rgba(245,245,220,0.80)] font-semibold">Span Detail</span>
          <button @click="selectedSpan = null" class="text-[rgba(245,245,220,0.40)] hover:text-[#f5f5dc]"><X :size="14" /></button>
        </div>
        <div class="grid grid-cols-2 gap-x-8 gap-y-2 font-mono mb-4">
          <div><span class="text-[rgba(245,245,220,0.40)]">span_id</span><br><span class="text-[#00d4ff]">{{ selectedSpan.span_id }}</span></div>
          <div><span class="text-[rgba(245,245,220,0.40)]">trace_id</span><br><span class="text-[rgba(245,245,220,0.50)]">{{ selectedSpan.trace_id }}</span></div>
          <div><span class="text-[rgba(245,245,220,0.40)]">name</span><br><span class="text-[#f5f5dc]">{{ selectedSpan.name }}</span></div>
          <div><span class="text-[rgba(245,245,220,0.40)]">kind</span><br><span :class="spanKindClass(selectedSpan.kind)">{{ selectedSpan.kind }}</span></div>
          <div><span class="text-[rgba(245,245,220,0.40)]">status</span><br><span :class="statusClass(selectedSpan.status.code)">{{ selectedSpan.status.code }}</span><span v-if="selectedSpan.status.message" class="ml-2 text-[rgba(245,245,220,0.50)]">{{ selectedSpan.status.message }}</span></div>
          <div v-if="selectedSpan.parent_span_id"><span class="text-[rgba(245,245,220,0.40)]">parent_span_id</span><br><span class="text-[rgba(245,245,220,0.50)]">{{ selectedSpan.parent_span_id }}</span></div>
        </div>
        <!-- Attributes -->
        <div v-if="Object.keys(selectedSpan.attributes).length > 0">
          <div class="text-[rgba(245,245,220,0.40)] mb-2">Attributes</div>
          <div class="bg-[#0f0f0f] rounded p-3 space-y-1">
            <div v-for="(val, key) in selectedSpan.attributes" :key="key" class="flex gap-4">
              <span class="text-[rgba(245,245,220,0.50)] w-40 flex-shrink-0">{{ key }}</span>
              <span class="text-[#00d4ff]">{{ val }}</span>
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useLogflayerStore } from '../../stores/logflayer'
import { useUpsidegateStore } from '../../stores/upsidegate'
import type { OtelSpan, SpanKind, SpanStatusCode } from '../../types'
import { RefreshCw, ChevronLeft, ChevronRight, X } from 'lucide-vue-next'

const store   = useLogflayerStore()
const ugStore = useUpsidegateStore()

const targetId    = ref('')
const page        = ref(1)
const limit       = 50
const kindFilter  = ref<SpanKind | ''>('')
const selectedSpan = ref<OtelSpan | null>(null)

const SPAN_KINDS: SpanKind[] = ['INTERNAL', 'CLIENT', 'SERVER', 'PRODUCER', 'CONSUMER']

const filteredSpans = computed(() => {
  if (!kindFilter.value) return ugStore.otelSpans
  return ugStore.otelSpans.filter(s => s.kind === kindFilter.value)
})

function spanBarClass(span: OtelSpan) {
  if (span.status.code === 'ERROR') return 'bg-[#dc143c]/60'
  if (span.kind === 'SERVER')       return 'bg-green-500/40'
  if (span.kind === 'CLIENT')       return 'bg-[#00d4ff]/40'
  if (span.kind === 'PRODUCER')     return 'bg-purple-500/40'
  if (span.kind === 'CONSUMER')     return 'bg-orange-500/40'
  return 'bg-[rgba(245,245,220,0.15)]'
}

function spanKindClass(kind: string) {
  const map: Record<string, string> = {
    INTERNAL: 'badge-yellow',
    CLIENT:   'badge-blue',
    SERVER:   'badge-green',
    PRODUCER: 'px-2 py-0.5 rounded text-xs bg-purple-500/20 text-purple-300 border border-purple-500/30',
    CONSUMER: 'px-2 py-0.5 rounded text-xs bg-orange-500/20 text-orange-300 border border-orange-500/30',
  }
  return map[kind] ?? 'badge-slate'
}

function statusClass(code: SpanStatusCode) {
  if (code === 'ERROR') return 'badge-red'
  if (code === 'OK')    return 'badge-green'
  return 'badge-slate'
}

async function loadPage(p: number) {
  page.value = p
  selectedSpan.value = null
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
