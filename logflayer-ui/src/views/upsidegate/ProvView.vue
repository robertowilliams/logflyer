<template>
  <div class="space-y-4">
    <!-- Sample picker -->
    <div class="flex flex-wrap gap-3 items-center">
      <select v-model="targetId" @change="loadPage(1)" class="input w-44">
        <option value="">All targets</option>
        <option v-for="c in store.sampleCollections" :key="c" :value="c">{{ c }}</option>
      </select>
      <button @click="loadPage(1)" class="btn-primary">↻ Refresh</button>
      <span class="ml-auto text-[rgba(245,245,220,0.40)] text-sm self-center">{{ ugStore.metadataTotal }} sample(s)</span>
    </div>

    <!-- Sample list -->
    <div class="card p-0 overflow-hidden">
      <div class="bg-[#0f0f0f] px-4 py-2 text-xs text-[rgba(245,245,220,0.40)] font-semibold uppercase tracking-wider border-b border-[#dc143c]/20">
        Select a sample to view PROV-O triples
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
            <th class="px-4 py-3">Analyzed At</th>
            <th class="px-4 py-3 text-right">Relations</th>
            <th class="px-4 py-3 text-right">PROV triples</th>
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
            <td class="px-4 py-2 text-[rgba(245,245,220,0.40)] text-xs">{{ fmt(m.analyzed_at) }}</td>
            <td class="px-4 py-2 text-right text-[rgba(245,245,220,0.70)]">{{ m.relation_count }}</td>
            <td class="px-4 py-2 text-right text-[rgba(245,245,220,0.50)]">{{ m.relation_count }}</td>
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

    <!-- PROV triples panel -->
    <div v-if="ugStore.selected">
      <!-- W3C PROV explanation -->
      <div class="card border-[#00d4ff]/15 bg-[#00d4ff]/5 mb-3">
        <div class="flex items-start gap-3">
          <span class="text-[#00d4ff] text-lg flex-shrink-0">ℹ</span>
          <div class="text-xs text-[rgba(245,245,220,0.60)] leading-relaxed">
            <strong class="text-[rgba(245,245,220,0.80)]">W3C PROV-O triples</strong> — synthesised client-side from relation edges.
            Each triple follows the RDF subject–predicate–object structure used in W3C provenance ontology.
            IDs shown are the first 16 chars of the full UUID entity IDs.
          </div>
        </div>
      </div>

      <!-- Predicate filter -->
      <div class="flex flex-wrap gap-2 mb-3">
        <button
          v-for="pred in PREDICATES"
          :key="pred"
          @click="predicateFilter = predicateFilter === pred ? '' : pred"
          :class="[
            'px-3 py-1 rounded-full text-xs font-mono border transition-all',
            predicateFilter === pred
              ? 'bg-[#00d4ff]/20 border-[#00d4ff]/60 text-[#f5f5dc]'
              : 'bg-[#1a1a1a] border-[#333] text-[rgba(245,245,220,0.60)] hover:border-[#00d4ff]/40',
          ]"
        >
          {{ pred }}
        </button>
        <button
          v-if="predicateFilter"
          @click="predicateFilter = ''"
          class="px-3 py-1 rounded-full text-xs border border-dashed border-[rgba(245,245,220,0.20)] text-[rgba(245,245,220,0.40)] hover:text-[#f5f5dc] transition-all"
        >Clear</button>
      </div>

      <!-- Triples table -->
      <div class="card p-0 overflow-hidden">
        <div class="bg-[#0f0f0f] px-4 py-2 text-xs text-[rgba(245,245,220,0.40)] font-semibold uppercase tracking-wider border-b border-[#dc143c]/20">
          PROV-O Triples — sample {{ ugStore.selected.sample_hash.slice(0, 16) }}… ({{ filteredTriples.length }})
        </div>
        <div v-if="filteredTriples.length === 0" class="px-4 py-8 text-center text-[rgba(245,245,220,0.30)] text-sm">
          No triples match the current filter.
        </div>
        <table v-else class="w-full text-xs">
          <thead class="bg-[#0f0f0f]">
            <tr class="text-[rgba(245,245,220,0.50)] text-left border-b border-[#dc143c]/20">
              <th class="px-3 py-2 w-5/12">Subject</th>
              <th class="px-3 py-2 w-2/12">Predicate</th>
              <th class="px-3 py-2 w-5/12">Object</th>
            </tr>
          </thead>
          <tbody class="divide-y divide-[#1a1a1a]">
            <tr v-for="(t, i) in filteredTriples" :key="i" class="hover:bg-[#dc143c]/5 transition-colors">
              <td class="px-3 py-2 font-mono text-[rgba(245,245,220,0.70)]">
                <span class="text-[rgba(245,245,220,0.30)]">prov:entity/</span>{{ t.subject.slice(0, 16) }}…
                <div class="text-[10px] text-[rgba(245,245,220,0.30)] mt-0.5">{{ entityLabel(t.subject) }}</div>
              </td>
              <td class="px-3 py-2 text-center">
                <span :class="predicateClass(t.predicate)">{{ t.predicate }}</span>
              </td>
              <td class="px-3 py-2 font-mono text-[rgba(245,245,220,0.70)]">
                <span class="text-[rgba(245,245,220,0.30)]">prov:entity/</span>{{ t.object.slice(0, 16) }}…
                <div class="text-[10px] text-[rgba(245,245,220,0.30)] mt-0.5">{{ entityLabel(t.object) }}</div>
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <!-- OTel Trace ID for this sample -->
      <div class="card mt-3 text-xs font-mono">
        <span class="text-[rgba(245,245,220,0.40)]">OTel Root Trace ID: </span>
        <span class="text-[#00d4ff]">{{ ugStore.selected.otel_trace_id }}</span>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useLogflayerStore } from '../../stores/logflayer'
import { useUpsidegateStore } from '../../stores/upsidegate'
import type { ProvPredicate } from '../../types'

const store   = useLogflayerStore()
const ugStore = useUpsidegateStore()

const targetId       = ref('')
const page           = ref(1)
const limit          = 50
const predicateFilter = ref('')

const PREDICATES: ProvPredicate[] = [
  'wasGeneratedBy', 'used', 'wasDerivedFrom',
  'wasAttributedTo', 'actedOnBehalfOf',
]

const filteredTriples = computed(() => {
  if (!predicateFilter.value) return ugStore.provTriples
  return ugStore.provTriples.filter(t => t.predicate === predicateFilter.value)
})

function fmt(ts: string) {
  try { return new Date(ts).toLocaleString() } catch { return ts }
}

/** Server-side PROV triples carry URIs like `ug:entity:{id}` / `ug:activity:{id}`
 *  / `ug:agent:{id}`; client-synthesised triples carry bare entity IDs.  Strip
 *  the prefix before looking up so both render correctly. */
function entityLabel(uriOrId: string): string {
  const id = uriOrId.replace(/^ug:(entity|activity|agent):/, '')
  const e = ugStore.entities.find(x => x.entity_id === id)
  return e ? (e.tool_name ?? e.entity_type) : '?'
}

function predicateClass(p: string) {
  const map: Record<string, string> = {
    wasGeneratedBy:  'badge-green',
    used:            'badge-blue',
    wasDerivedFrom:  'badge-yellow',
    wasAttributedTo: 'badge-yellow',
    actedOnBehalfOf: 'badge-blue',
  }
  return map[p] ?? 'badge-slate'
}

async function loadPage(p: number) {
  page.value = p
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
