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
      <button @click="load(1)" class="btn-primary">↻ Refresh</button>
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
    <div v-if="selected !== null && filtered[selected] != null" class="card bg-[#0a0a0a] border-[#dc143c]/30 space-y-4">
      <div class="flex justify-between items-start">
        <div>
          <span :class="severityClass(filtered[selected!].severity)" class="text-sm mr-3">{{ filtered[selected!].severity.toUpperCase() }}</span>
          <span class="text-[rgba(245,245,220,0.80)] font-semibold text-sm">{{ filtered[selected!].target_id }}</span>
          <span class="text-[rgba(245,245,220,0.40)] text-xs ml-3">{{ fmt(filtered[selected!].classified_at) }}</span>
        </div>
        <button @click="selected = null" class="text-[rgba(245,245,220,0.40)] hover:text-[#f5f5dc] transition-colors flex-shrink-0 ml-4">✕</button>
      </div>

      <!-- Summary -->
      <div>
        <div class="text-xs text-[rgba(245,245,220,0.50)] uppercase tracking-wide mb-1">Summary</div>
        <p class="text-[rgba(245,245,220,0.80)] text-sm leading-relaxed">{{ filtered[selected!].summary }}</p>
      </div>

      <!-- Key findings -->
      <div v-if="filtered[selected!].key_findings.length > 0">
        <div class="text-xs text-[rgba(245,245,220,0.50)] uppercase tracking-wide mb-2">Key Findings</div>
        <div class="space-y-2">
          <div
            v-for="(f, fi) in filtered[selected!].key_findings"
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
      <div v-if="filtered[selected!].recommendations.length > 0">
        <div class="text-xs text-[rgba(245,245,220,0.50)] uppercase tracking-wide mb-2">Recommendations</div>
        <ul class="space-y-1">
          <li
            v-for="(r, ri) in filtered[selected!].recommendations"
            :key="ri"
            class="text-sm text-[rgba(245,245,220,0.70)] flex items-start gap-2"
          >
            <span class="text-[#00d4ff] mt-0.5 flex-shrink-0">→</span>{{ r }}
          </li>
        </ul>
      </div>

      <!-- Footer metadata -->
      <div class="border-t border-[#dc143c]/10 pt-3 flex flex-wrap gap-4 text-xs text-[rgba(245,245,220,0.30)]">
        <span>Model: <span class="text-[rgba(245,245,220,0.50)]">{{ filtered[selected!].model }}</span></span>
        <span>In: <span class="text-[rgba(245,245,220,0.50)]">{{ filtered[selected!].input_tokens }} tok</span></span>
        <span>Out: <span class="text-[rgba(245,245,220,0.50)]">{{ filtered[selected!].output_tokens }} tok</span></span>
        <span>Confidence: <span class="text-[rgba(245,245,220,0.50)]">{{ (filtered[selected!].confidence * 100).toFixed(0) }}%</span></span>
        <router-link
          :to="`/samples?target_id=${filtered[selected!].target_id}`"
          class="text-[#00d4ff] hover:underline ml-auto"
        >
          View source sample →
        </router-link>
      </div>
    </div>

    <!-- Pagination -->
    <div class="flex items-center justify-between text-sm text-[rgba(245,245,220,0.40)]">
      <span>Page {{ page }}</span>
      <div class="flex gap-2">
        <button :disabled="page <= 1" @click="load(page - 1)" class="btn-secondary py-1 disabled:opacity-40">← Prev</button>
        <button :disabled="page * limit >= store.classificationsTotal" @click="load(page + 1)" class="btn-secondary py-1 disabled:opacity-40">Next →</button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useLogflayerStore } from '../stores/logflayer'

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
