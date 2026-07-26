<template>
  <div class="space-y-4">
    <!-- Filters -->
    <div class="flex flex-wrap gap-3">
      <select v-model="targetId" @change="load(1)" class="input w-52">
        <option value="">All targets</option>
        <option v-for="c in store.sampleCollections" :key="c" :value="c">{{ c }}</option>
      </select>
      <select v-model="severityFilter" @change="load(1)" class="input w-36">
        <option value="">All severities</option>
        <option value="critical">Critical</option>
        <option value="warning">Warning</option>
        <option value="info">Info</option>
        <option value="normal">Normal</option>
      </select>
      <button @click="load(1)" class="btn-primary inline-flex items-center gap-1.5"><RefreshCw :size="14" />Refresh</button>
      <span class="ml-auto text-[rgba(245,245,220,0.40)] text-sm self-center">{{ store.classificationsTotal }} total</span>
    </div>

    <!-- Table -->
    <div class="card p-0 overflow-hidden">
      <table class="w-full text-sm">
        <thead class="bg-[#0f0f0f]">
          <tr class="text-[rgba(245,245,220,0.50)] text-left border-b border-[#dc143c]/20">
            <th class="px-4 py-3">Classified At</th>
            <th class="px-4 py-3">Target</th>
            <th class="px-4 py-3">Severity</th>
            <th class="px-4 py-3">Categories</th>
            <th class="px-4 py-3">Summary</th>
            <th class="px-4 py-3">Confidence</th>
          </tr>
        </thead>
        <tbody class="divide-y divide-[#1a1a1a]">
          <tr v-if="store.loading">
            <td colspan="6" class="px-4 py-6 text-center text-[rgba(245,245,220,0.40)]">Loading…</td>
          </tr>
          <tr v-else-if="filtered.length === 0">
            <td colspan="6" class="px-4 py-6 text-center text-[rgba(245,245,220,0.30)]">No classifications found.</td>
          </tr>
          <tr
            v-for="(c, i) in filtered"
            :key="c.sample_hash"
            class="hover:bg-[#dc143c]/5 transition-colors cursor-pointer"
            :class="{ 'bg-[#dc143c]/10': selected === i }"
            @click="selected = selected === i ? null : i"
          >
            <td class="px-4 py-2 text-[rgba(245,245,220,0.40)] text-xs font-mono">{{ fmt(c.classified_at) }}</td>
            <td class="px-4 py-2 text-[#dc143c] font-mono text-xs">{{ c.target_id }}</td>
            <td class="px-4 py-2">
              <span :class="severityClass(c.severity)">{{ c.severity }}</span>
            </td>
            <td class="px-4 py-2">
              <span
                v-for="cat in c.categories.slice(0, 3)"
                :key="cat"
                class="badge-slate mr-1 text-[10px]"
              >{{ cat }}</span>
            </td>
            <td class="px-4 py-2 text-[rgba(245,245,220,0.70)] text-xs max-w-xs truncate" :title="c.summary">
              {{ c.summary }}
            </td>
            <td class="px-4 py-2 text-[rgba(245,245,220,0.50)] text-xs">
              {{ (c.confidence * 100).toFixed(0) }}%
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <!-- Detail drawer -->
    <div v-if="selectedRecord" class="card bg-[#0a0a0a] border-[#dc143c]/30 space-y-4">
      <div class="flex justify-between items-start">
        <div>
          <span :class="severityClass(selectedRecord.severity)" class="text-sm mr-3">{{ selectedRecord.severity.toUpperCase() }}</span>
          <span class="text-[rgba(245,245,220,0.80)] font-semibold text-sm">{{ selectedRecord.target_id }}</span>
          <span class="text-[rgba(245,245,220,0.40)] text-xs ml-3">{{ fmt(selectedRecord.classified_at) }}</span>
        </div>
        <button @click="selected = null" class="text-[rgba(245,245,220,0.40)] hover:text-[#f5f5dc] transition-colors flex-shrink-0 ml-4"><X :size="16" /></button>
      </div>

      <!-- Summary -->
      <div>
        <div class="text-xs text-[rgba(245,245,220,0.50)] uppercase tracking-wide mb-1">Summary</div>
        <p class="text-[rgba(245,245,220,0.80)] text-sm leading-relaxed">{{ selectedRecord.summary }}</p>
      </div>

      <!-- Key findings -->
      <div v-if="selectedRecord.key_findings.length > 0">
        <div class="text-xs text-[rgba(245,245,220,0.50)] uppercase tracking-wide mb-2">Key Findings</div>
        <div class="space-y-2">
          <div
            v-for="(f, fi) in selectedRecord.key_findings"
            :key="fi"
            class="bg-[#0f0f0f] rounded p-3 border border-[#dc143c]/10"
          >
            <div class="flex items-center gap-2 mb-1">
              <span :class="findingSeverityClass(f.severity)" class="text-[10px]">{{ f.severity }}</span>
              <span class="text-[rgba(245,245,220,0.80)] text-xs font-medium">{{ f.pattern }}</span>
              <span class="text-[rgba(245,245,220,0.40)] text-xs ml-auto">×{{ f.count }}</span>
            </div>
            <pre v-if="f.example" class="text-[10px] text-[#00d4ff] mt-1 overflow-x-auto whitespace-pre-wrap">{{ f.example }}</pre>
          </div>
        </div>
      </div>

      <!-- Recommendations -->
      <div v-if="selectedRecord.recommendations.length > 0">
        <div class="text-xs text-[rgba(245,245,220,0.50)] uppercase tracking-wide mb-2">Recommendations</div>
        <ul class="space-y-1">
          <li
            v-for="(r, ri) in selectedRecord.recommendations"
            :key="ri"
            class="text-sm text-[rgba(245,245,220,0.70)] flex items-start gap-2"
          >
            <ArrowRight :size="14" class="text-[#00d4ff] mt-0.5 flex-shrink-0" />{{ r }}
          </li>
        </ul>
      </div>

      <!-- Footer metadata -->
      <div class="border-t border-[#dc143c]/10 pt-3 flex flex-wrap gap-4 text-xs text-[rgba(245,245,220,0.30)]">
        <span>Model: <span class="text-[rgba(245,245,220,0.50)]">{{ selectedRecord.model }}</span></span>
        <span>In: <span class="text-[rgba(245,245,220,0.50)]">{{ selectedRecord.input_tokens }} tok</span></span>
        <span>Out: <span class="text-[rgba(245,245,220,0.50)]">{{ selectedRecord.output_tokens }} tok</span></span>
        <span>Confidence: <span class="text-[rgba(245,245,220,0.50)]">{{ (selectedRecord.confidence * 100).toFixed(0) }}%</span></span>
        <router-link
          :to="`/samples?target_id=${selectedRecord.target_id}`"
          class="text-[#00d4ff] hover:underline ml-auto inline-flex items-center gap-1"
        >
          View source sample<ArrowRight :size="14" />
        </router-link>
      </div>
    </div>

    <!-- Pagination -->
    <div class="flex items-center justify-between text-sm text-[rgba(245,245,220,0.40)]">
      <span>Page {{ page }}</span>
      <div class="flex gap-2">
        <button :disabled="page <= 1" @click="load(page - 1)" class="btn-secondary py-1 disabled:opacity-40 inline-flex items-center gap-1"><ChevronLeft :size="14" />Prev</button>
        <button :disabled="page * limit >= store.classificationsTotal" @click="load(page + 1)" class="btn-secondary py-1 disabled:opacity-40 inline-flex items-center gap-1">Next<ChevronRight :size="14" /></button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { RefreshCw, ChevronLeft, ChevronRight, ArrowRight, X } from 'lucide-vue-next'
import { useLogflayerStore } from '../stores/logflayer'
import type { ClassificationRecord } from '../types'

const store = useLogflayerStore()
const targetId = ref('')
const severityFilter = ref('')
const page = ref(1)
const limit = 50
const selected = ref<number | null>(null)

// Client-side severity filter on top of the server-side target filter.
const filtered = computed(() => {
  if (!severityFilter.value) return store.classifications
  return store.classifications.filter(c => c.severity === severityFilter.value)
})

/** The record the detail drawer is showing, resolved once.
 *
 *  `selected` is an index, and indexing an array yields `T | undefined` under
 *  `noUncheckedIndexedAccess` — so reading `filtered[selected]` directly in the
 *  template needed a non-null assertion at every one of its fourteen use sites.
 *  Resolving it here instead means the template does a single truthiness check
 *  and every field access below it is soundly narrowed.
 *
 *  Behaviour is unchanged: the old template already guarded on
 *  `filtered[selected] != null`, and every filter change routes through `load`,
 *  which resets `selected`. This is a type-level cleanup, not a bug fix. */
const selectedRecord = computed<ClassificationRecord | null>(() =>
  selected.value === null ? null : filtered.value[selected.value] ?? null,
)

function fmt(ts: string) {
  try { return new Date(ts).toLocaleString() } catch { return ts }
}

function severityClass(s: string) {
  if (s === 'critical') return 'badge-red'
  if (s === 'warning')  return 'badge-yellow'
  if (s === 'info')     return 'badge-blue'
  return 'badge-slate'
}

function findingSeverityClass(s: string) {
  if (s === 'critical') return 'badge-red'
  if (s === 'warning')  return 'badge-yellow'
  return 'badge-blue'
}

async function load(p: number) {
  page.value = p
  selected.value = null
  await store.fetchClassifications({ target_id: targetId.value || undefined, limit, page: p - 1 })
}

onMounted(async () => {
  await store.fetchSampleCollections()
  await load(1)
})
</script>
