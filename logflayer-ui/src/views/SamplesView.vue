<template>
  <div class="space-y-4">
    <!-- Filters -->
    <div class="flex flex-wrap gap-3">
      <select v-model="targetId" @change="load(1)" class="input w-52">
        <option value="">All targets</option>
        <option v-for="c in store.sampleCollections" :key="c" :value="c">{{ c }}</option>
      </select>
      <button @click="load(1)" class="btn-primary">↻ Refresh</button>
      <span class="ml-auto text-[rgba(245,245,220,0.40)] text-sm self-center">{{ store.samplesTotal }} total records</span>
    </div>

    <!-- Table -->
    <div class="card p-0 overflow-hidden">
      <table class="w-full text-sm">
        <thead class="bg-[#0f0f0f]">
          <tr class="text-[rgba(245,245,220,0.50)] text-left border-b border-[#dc143c]/20">
            <th class="px-4 py-3">Timestamp</th>
            <th class="px-4 py-3">Target</th>
            <th class="px-4 py-3">Source File</th>
            <th class="px-4 py-3">Mode</th>
            <th class="px-4 py-3">Lines</th>
            <th class="px-4 py-3">Size</th>
            <th class="px-4 py-3">Status</th>
            <th class="px-4 py-3"></th>
          </tr>
        </thead>
        <tbody class="divide-y divide-[#1a1a1a]">
          <tr v-if="store.loading">
            <td colspan="8" class="px-4 py-6 text-center text-[rgba(245,245,220,0.40)]">Loading…</td>
          </tr>
          <tr v-else-if="store.samples.length === 0">
            <td colspan="8" class="px-4 py-6 text-center text-[rgba(245,245,220,0.30)]">No samples found.</td>
          </tr>
          <tr
            v-for="(s, i) in store.samples"
            :key="i"
            class="hover:bg-[#dc143c]/5 transition-colors cursor-pointer group"
            @click="selected = selected === i ? null : i"
          >
            <td class="px-4 py-2 text-[rgba(245,245,220,0.40)] text-xs font-mono">{{ fmt(s.timestamp) }}</td>
            <td class="px-4 py-2 text-[#dc143c] font-mono text-xs">{{ s.target_id }}</td>
            <td class="px-4 py-2 text-[rgba(245,245,220,0.70)] text-xs truncate max-w-xs" :title="s.source_file">{{ s.source_file }}</td>
            <td class="px-4 py-2"><span class="badge-blue">{{ s.sampling_mode }}</span></td>
            <td class="px-4 py-2 text-[rgba(245,245,220,0.50)]">{{ s.line_count ?? '—' }}</td>
            <td class="px-4 py-2 text-[rgba(245,245,220,0.50)]">{{ fmtSize(s.file_size_bytes) }}</td>
            <td class="px-4 py-2">
              <span :class="statusClass(s.processing_status)">{{ s.processing_status }}</span>
            </td>
            <td class="px-4 py-2 text-right" @click.stop>
              <button
                @click="openDeleteDialog(s)"
                class="text-[rgba(245,245,220,0.25)] hover:text-[#ff6b8a] transition-colors text-sm"
                title="Delete sample"
              >🗑</button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <!-- Content drawer -->
    <div v-if="selected !== null && store.samples[selected] != null" class="card bg-[#0a0a0a] border-[#dc143c]/30">
      <div class="flex justify-between items-center mb-3">
        <div>
          <span class="text-[rgba(245,245,220,0.80)] font-semibold text-sm">Sample Content</span>
          <span class="text-[rgba(245,245,220,0.40)] text-xs ml-3">{{ store.samples[selected!].source_file }}</span>
        </div>
        <div class="flex items-center gap-3">
          <button
            @click="openDeleteDialog(store.samples[selected!])"
            class="text-[rgba(245,245,220,0.30)] hover:text-[#ff6b8a] transition-colors text-xs flex items-center gap-1"
          >🗑 Delete</button>
          <button @click="selected = null" class="text-[rgba(245,245,220,0.40)] hover:text-[#f5f5dc] transition-colors">✕</button>
        </div>
      </div>
      <pre v-if="store.samples[selected!].sample_content" class="text-xs text-[#00d4ff] overflow-auto whitespace-pre-wrap max-h-[500px]">{{ store.samples[selected!].sample_content }}</pre>
      <p v-else class="text-xs text-[rgba(245,245,220,0.30)] italic">No content captured.</p>
      <div v-if="store.samples[selected!].error_details" class="mt-3 text-[#ff6b8a] text-xs">
        Error: {{ store.samples[selected!].error_details }}
      </div>
    </div>

    <!-- Delete dialog -->
    <div
      v-if="deleteTarget"
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
      @click.self="deleteTarget = null"
    >
      <div class="bg-[#0f0f0f] border border-[#dc143c]/40 rounded-xl p-6 w-full max-w-md shadow-2xl space-y-4">
        <h2 class="text-[#f5f5dc] font-semibold">Delete sample</h2>
        <div class="text-xs text-[rgba(245,245,220,0.50)] font-mono space-y-1">
          <div><span class="text-[rgba(245,245,220,0.30)]">hash</span> {{ deleteTarget.sample_hash }}</div>
          <div><span class="text-[rgba(245,245,220,0.30)]">file</span> {{ deleteTarget.source_file }}</div>
        </div>
        <div>
          <label class="block text-xs text-[rgba(245,245,220,0.50)] mb-1">Reason <span class="text-[#ff6b8a]">*</span></label>
          <textarea
            v-model="deleteReason"
            rows="3"
            placeholder="Why is this sample being deleted?"
            class="input w-full resize-none text-sm"
            @keydown.esc="deleteTarget = null"
          />
        </div>
        <div v-if="deleteError" class="text-[#ff6b8a] text-xs">{{ deleteError }}</div>
        <div class="flex justify-end gap-3">
          <button @click="deleteTarget = null" class="btn-secondary text-sm">Cancel</button>
          <button
            @click="confirmDelete"
            :disabled="deleting || !deleteReason.trim()"
            class="btn-primary text-sm bg-[#dc143c]/80 hover:bg-[#dc143c] disabled:opacity-40"
          >{{ deleting ? 'Deleting…' : 'Delete' }}</button>
        </div>
      </div>
    </div>

    <!-- Pagination -->
    <div class="flex items-center justify-between text-sm text-[rgba(245,245,220,0.40)]">
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
import { client } from '../api/client'
import type { SampleRecord } from '../types'

const store = useLogflayerStore()
const targetId = ref('')
const page = ref(1)
const limit = 50
const selected = ref<number | null>(null)

// ── Delete dialog state ──────────────────────────────────────────────────────
const deleteTarget = ref<SampleRecord | null>(null)
const deleteReason = ref('')
const deleteError  = ref('')
const deleting     = ref(false)

function openDeleteDialog(s: SampleRecord) {
  deleteTarget.value = s
  deleteReason.value = ''
  deleteError.value  = ''
}

async function confirmDelete() {
  if (!deleteTarget.value || !deleteReason.value.trim()) return
  deleting.value = true
  deleteError.value = ''
  try {
    await client.deleteSample(
      deleteTarget.value.sample_hash,
      deleteTarget.value.target_id,
      deleteReason.value.trim(),
    )
    deleteTarget.value = null
    selected.value = null
    await load(page.value)
  } catch (e: any) {
    deleteError.value = e.response?.data?.error ?? e.message ?? 'Delete failed'
  } finally {
    deleting.value = false
  }
}

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
