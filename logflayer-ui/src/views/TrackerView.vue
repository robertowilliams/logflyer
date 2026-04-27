<template>
  <div class="space-y-4">
    <!-- Filters -->
    <div class="flex flex-wrap gap-3">
      <input v-model="search" @input="onSearch" type="text" placeholder="Search messages…" class="input flex-1 min-w-48" />
      <select v-model="level" @change="load(1)" class="input w-36">
        <option value="">All levels</option>
        <option value="info">info</option>
        <option value="warn">warn</option>
        <option value="error">error</option>
        <option value="debug">debug</option>
      </select>
      <button @click="load(1)" class="btn-primary">Search</button>
    </div>

    <!-- Table -->
    <div class="card p-0 overflow-hidden">
      <table class="w-full text-sm">
        <thead>
          <tr class="text-slate-400 text-left border-b border-slate-700">
            <th class="px-4 py-3 w-40">Timestamp</th>
            <th class="px-4 py-3 w-20">Level</th>
            <th class="px-4 py-3">Message</th>
          </tr>
        </thead>
        <tbody class="divide-y divide-slate-700">
          <tr v-if="store.loading"><td colspan="3" class="px-4 py-6 text-center text-slate-400">Loading…</td></tr>
          <tr v-else-if="store.trackingRecords.length === 0">
            <td colspan="3" class="px-4 py-6 text-center text-slate-500">No records found.</td>
          </tr>
          <tr
            v-for="(r, i) in store.trackingRecords"
            :key="i"
            class="hover:bg-slate-750 transition-colors cursor-pointer"
            @click="selected = selected === i ? null : i"
          >
            <td class="px-4 py-2 text-slate-400 text-xs font-mono">{{ fmt(r.timestamp) }}</td>
            <td class="px-4 py-2">
              <span :class="levelClass(r.level)">{{ r.level || '—' }}</span>
            </td>
            <td class="px-4 py-2 text-slate-200">{{ r.message || JSON.stringify(r) }}</td>
          </tr>
        </tbody>
      </table>
    </div>

    <!-- Detail drawer -->
    <div v-if="selected !== null && store.trackingRecords[selected]" class="card bg-slate-900">
      <div class="flex justify-between items-center mb-3">
        <span class="text-slate-300 font-semibold text-sm">Record Detail</span>
        <button @click="selected = null" class="text-slate-500 hover:text-white">✕</button>
      </div>
      <pre class="text-xs text-green-300 overflow-x-auto">{{ JSON.stringify(store.trackingRecords[selected], null, 2) }}</pre>
    </div>

    <!-- Pagination -->
    <div class="flex items-center justify-between text-sm text-slate-400">
      <span>{{ store.trackingTotal }} total records</span>
      <div class="flex gap-2">
        <button :disabled="page <= 1" @click="load(page - 1)" class="btn-secondary py-1 disabled:opacity-40">← Prev</button>
        <span class="px-3 py-1">Page {{ page }}</span>
        <button :disabled="page * limit >= store.trackingTotal" @click="load(page + 1)" class="btn-secondary py-1 disabled:opacity-40">Next →</button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useLogflayerStore } from '../stores/logflayer'

const store = useLogflayerStore()
const search = ref('')
const level = ref('')
const page = ref(1)
const limit = 50
const selected = ref<number | null>(null)
let searchTimer: ReturnType<typeof setTimeout> | null = null

function levelClass(lvl?: string) {
  switch ((lvl || '').toLowerCase()) {
    case 'error': return 'badge-red'
    case 'warn':  return 'badge-yellow'
    case 'debug': return 'badge-slate'
    default:      return 'badge-blue'
  }
}

function fmt(ts?: string) {
  if (!ts) return '—'
  try { return new Date(ts).toLocaleString() } catch { return ts }
}

function onSearch() {
  if (searchTimer) clearTimeout(searchTimer)
  searchTimer = setTimeout(() => load(1), 400)
}

async function load(p: number) {
  page.value = p
  selected.value = null
  await store.fetchTracking({ limit, page: p, search: search.value, level: level.value })
}

onMounted(() => load(1))
</script>
