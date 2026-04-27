<template>
  <div class="space-y-4">
    <!-- Filters -->
    <div class="flex flex-wrap gap-3">
      <select v-model="targetId" @change="load(1)" class="input w-52">
        <option value="">All targets</option>
        <option v-for="c in store.sampleCollections" :key="c" :value="c">{{ c }}</option>
      </select>
      <button @click="load(1)" class="btn-primary">↻ Refresh</button>
      <span class="ml-auto text-slate-400 text-sm self-center">{{ store.samplesTotal }} total records</span>
    </div>

    <!-- Table -->
    <div class="card p-0 overflow-hidden">
      <table class="w-full text-sm">
        <thead>
          <tr class="text-slate-400 text-left border-b border-slate-700">
            <th class="px-4 py-3">Timestamp</th>
            <th class="px-4 py-3">Target</th>
            <th class="px-4 py-3">Source File</th>
            <th class="px-4 py-3">Mode</th>
            <th class="px-4 py-3">Lines</th>
            <th class="px-4 py-3">Size</th>
            <th class="px-4 py-3">Status</th>
          </tr>
        </thead>
        <tbody class="divide-y divide-slate-700">
          <tr v-if="store.loading"><td colspan="7" class="px-4 py-6 text-center text-slate-400">Loading…</td></tr>
          <tr v-else-if="store.samples.length === 0">
            <td colspan="7" class="px-4 py-6 text-center text-slate-500">No samples found.</td>
          </tr>
          <tr
            v-for="(s, i) in store.samples"
            :key="i"
            class="hover:bg-slate-750 transition-colors cursor-pointer"
            @click="selected = selected === i ? null : i"
          >
            <td class="px-4 py-2 text-slate-400 text-xs font-mono">{{ fmt(s.timestamp) }}</td>
            <td class="px-4 py-2 text-primary-300 font-mono text-xs">{{ s.target_id }}</td>
            <td class="px-4 py-2 text-slate-300 text-xs truncate max-w-xs" :title="s.source_file">{{ s.source_file }}</td>
            <td class="px-4 py-2"><span class="badge-blue">{{ s.sampling_mode }}</span></td>
            <td class="px-4 py-2 text-slate-400">{{ s.line_count ?? '—' }}</td>
            <td class="px-4 py-2 text-slate-400">{{ fmtSize(s.file_size_bytes) }}</td>
            <td class="px-4 py-2">
              <span :class="statusClass(s.processing_status)">{{ s.processing_status }}</span>
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <!-- Content drawer -->
    <div v-if="selected !== null && store.samples[selected]" class="card bg-slate-900">
      <div class="flex justify-between items-center mb-3">
        <div>
          <span class="text-slate-300 font-semibold text-sm">Sample Content</span>
          <span class="text-slate-500 text-xs ml-3">{{ store.samples[selected].source_file }}</span>
        </div>
        <button @click="selected = null" class="text-slate-500 hover:text-white">✕</button>
      </div>
      <pre class="text-xs text-green-300 overflow-x-auto whitespace-pre-wrap max-h-80">{{ store.samples[selected].sample_content }}</pre>
      <div v-if="store.samples[selected].error_details" class="mt-3 text-red-400 text-xs">
        Error: {{ store.samples[selected].error_details }}
      </div>
    </div>

    <!-- Pagination -->
    <div class="flex items-center justify-between text-sm text-slate-400">
      <span>Page {{ page }}</span>
      <div class="flex gap-2">
        <button :disabled="page <= 1" @click="load(page - 1)" class="btn-secondary py-1 disabled:opacity-40">← Prev</button>
        <button :disabled="page * limit >= store.samplesTotal" @click="load(page + 1)" class="btn-secondary py-1 disabled:opacity-40">Next →</button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useLogflayerStore } from '../stores/logflayer'

const store = useLogflayerStore()
const targetId = ref('')
const page = ref(1)
const limit = 50
const selected = ref<number | null>(null)

function fmt(ts: string) {
  try { return new Date(ts).toLocaleString() } catch { return ts }
}
function fmtSize(bytes?: number) {
  if (bytes == null) return '—'
  if (bytes < 1024) return `${bytes}B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`
  return `${(bytes / 1024 / 1024).toFixed(1)}MB`
}
function statusClass(s: string) {
  if (s === 'stored') return 'badge-green'
  if (s === 'error') return 'badge-red'
  if (s === 'empty') return 'badge-yellow'
  return 'badge-slate'
}

async function load(p: number) {
  page.value = p
  selected.value = null
  await store.fetchSamples({ target_id: targetId.value || undefined, limit, page: p })
}

onMounted(async () => {
  await store.fetchSampleCollections()
  await load(1)
})
</script>
