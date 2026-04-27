<template>
  <div class="space-y-4">
    <!-- Controls -->
    <div class="flex items-center gap-4">
      <div class="flex items-center gap-2">
        <label class="text-slate-400 text-sm">Lines:</label>
        <select v-model="lineCount" class="input w-28 py-1">
          <option :value="100">100</option>
          <option :value="200">200</option>
          <option :value="500">500</option>
          <option :value="1000">1000</option>
        </select>
      </div>
      <button @click="load" class="btn-primary py-1">↻ Refresh</button>
      <label class="flex items-center gap-2 text-slate-400 text-sm cursor-pointer">
        <input type="checkbox" v-model="autoRefresh" class="rounded" />
        Auto-refresh (5s)
      </label>
      <div v-if="store.logFile" class="text-slate-500 text-xs ml-auto truncate">{{ store.logFile }}</div>
    </div>

    <!-- Filter -->
    <input v-model="filter" type="text" placeholder="Filter log lines…" class="input" />

    <!-- Log output -->
    <div
      ref="logBox"
      class="bg-slate-950 border border-slate-700 rounded-lg p-4 font-mono text-xs
             h-[65vh] overflow-y-auto whitespace-pre-wrap leading-5"
    >
      <div v-if="store.loading && lines.length === 0" class="text-slate-500">Loading…</div>
      <div v-else-if="lines.length === 0" class="text-slate-500">No log lines found.</div>
      <div
        v-for="(line, i) in lines"
        :key="i"
        :class="lineClass(line)"
      >{{ line }}</div>
    </div>

    <div class="text-slate-500 text-xs">{{ lines.length }} lines shown</div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch, nextTick } from 'vue'
import { useLogflayerStore } from '../stores/logflayer'

const store = useLogflayerStore()
const lineCount = ref(200)
const filter = ref('')
const autoRefresh = ref(false)
const logBox = ref<HTMLElement | null>(null)
let timer: ReturnType<typeof setInterval> | null = null

const lines = computed(() =>
  filter.value
    ? store.logLines.filter(l => l.toLowerCase().includes(filter.value.toLowerCase()))
    : store.logLines
)

function lineClass(line: string) {
  const lower = line.toLowerCase()
  if (lower.includes('"level":"error"') || lower.includes('error') || lower.includes('err '))
    return 'text-red-400'
  if (lower.includes('"level":"warn"') || lower.includes('warn'))
    return 'text-yellow-400'
  if (lower.includes('"level":"debug"') || lower.includes('debug'))
    return 'text-slate-500'
  return 'text-green-300'
}

async function load() {
  await store.fetchLogs(lineCount.value)
  await nextTick()
  if (logBox.value) logBox.value.scrollTop = logBox.value.scrollHeight
}

watch(autoRefresh, (val) => {
  if (val) {
    timer = setInterval(load, 5000)
  } else {
    if (timer) clearInterval(timer)
  }
})

watch(lineCount, load)

onMounted(load)
onUnmounted(() => { if (timer) clearInterval(timer) })
</script>
