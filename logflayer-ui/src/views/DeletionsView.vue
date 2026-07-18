<template>
  <div class="space-y-4">
    <!-- Header -->
    <div class="flex flex-wrap gap-3 items-center">
      <button @click="load(1)" class="btn-primary">↻ Refresh</button>
      <span class="ml-auto text-[rgba(245,245,220,0.40)] text-sm self-center">{{ total }} total deletions</span>
    </div>

    <!-- Table -->
    <div class="card p-0 overflow-hidden">
      <table class="w-full text-sm">
        <thead class="bg-[#0f0f0f]">
          <tr class="text-[rgba(245,245,220,0.50)] text-left border-b border-[#dc143c]/20">
            <th class="px-4 py-3">Deleted At</th>
            <th class="px-4 py-3">Target</th>
            <th class="px-4 py-3">Sample Hash</th>
            <th class="px-4 py-3">Reason</th>
          </tr>
        </thead>
        <tbody class="divide-y divide-[#1a1a1a]">
          <tr v-if="loading">
            <td colspan="4" class="px-4 py-6 text-center text-[rgba(245,245,220,0.40)]">Loading…</td>
          </tr>
          <tr v-else-if="records.length === 0">
            <td colspan="4" class="px-4 py-6 text-center text-[rgba(245,245,220,0.30)]">No deletions recorded yet.</td>
          </tr>
          <tr
            v-for="(r, i) in records"
            :key="i"
            class="hover:bg-[#dc143c]/5 transition-colors"
          >
            <td class="px-4 py-2 text-[rgba(245,245,220,0.40)] text-xs font-mono whitespace-nowrap">{{ fmt(r.deleted_at) }}</td>
            <td class="px-4 py-2 text-[#dc143c] font-mono text-xs">{{ r.target_id }}</td>
            <td class="px-4 py-2 text-[rgba(245,245,220,0.40)] font-mono text-xs truncate max-w-[220px]" :title="r.sample_hash">{{ r.sample_hash }}</td>
            <td class="px-4 py-2 text-[rgba(245,245,220,0.75)] text-xs">{{ r.reason }}</td>
          </tr>
        </tbody>
      </table>
    </div>

    <!-- Pagination -->
    <div class="flex items-center justify-between text-sm text-[rgba(245,245,220,0.40)]">
      <span>Page {{ page }}</span>
      <div class="flex gap-2">
        <button :disabled="page <= 1" @click="load(page - 1)" class="btn-secondary py-1 disabled:opacity-40">← Prev</button>
        <button :disabled="page * limit >= total" @click="load(page + 1)" class="btn-secondary py-1 disabled:opacity-40">Next →</button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { client } from '../api/client'
import type { DeletionRecord } from '../types'

const records = ref<DeletionRecord[]>([])
const total   = ref(0)
const page    = ref(1)
const limit   = 50
const loading = ref(false)

function fmt(ts: string) {
  try { return new Date(ts).toLocaleString() } catch { return ts }
}

async function load(p: number) {
  page.value    = p
  loading.value = true
  try {
    const res = await client.getDeletions({ limit, page: p })
    records.value = res.records
    total.value   = res.total
  } finally {
    loading.value = false
  }
}

onMounted(() => load(1))
</script>
