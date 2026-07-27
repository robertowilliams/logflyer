<!--
  Actor browser — the participants in the interaction graph (Stage 12).

  Agents, skills and resources are promoted out of event attributes (`model_id`,
  `tool_name`, `mcp_server_id`) into nodes of their own, shared across samples.
  That sharing is what makes "which agents used this skill" answerable at all.

  Known limitation worth keeping in mind while reading these counts: an actor id
  is derived from `(kind, name)` and is *not* scoped to a target, so two
  unrelated systems that both have a tool called `search` appear here as one
  node with their event counts summed. See STAGES_11_13_REVIEW.md §2.2.
-->
<template>
  <div class="space-y-4">
    <!-- ── Filters ──────────────────────────────────────────────────────── -->
    <div class="flex flex-wrap gap-3 items-center">
      <select v-model="kind" class="input w-44" @change="loadPage(1)">
        <option value="">All kinds</option>
        <option value="agent">Agents</option>
        <option value="skill">Skills</option>
        <option value="resource">Resources</option>
      </select>

      <input
        v-model="taskId"
        class="input flex-1 min-w-0 font-mono text-sm"
        placeholder="Filter by task id — who worked on this"
        @keyup.enter="loadPage(1)"
      >

      <button class="btn-primary inline-flex items-center gap-1.5" @click="loadPage(page)">
        <RefreshCw :size="14" /> Refresh
      </button>

      <span class="ml-auto text-[rgba(245,245,220,0.40)] text-sm self-center">
        {{ ugStore.actorTotal }} actor{{ ugStore.actorTotal === 1 ? '' : 's' }}
      </span>
    </div>

    <div
      v-if="ugStore.taskError"
      class="card border-[#dc143c]/60 bg-[#dc143c]/10 text-[#ff6b8a] text-sm flex items-center justify-between"
    >
      <span>{{ ugStore.taskError }}</span>
      <button class="text-[#ff6b8a] hover:text-[#f5f5dc]" @click="ugStore.clearTaskError()">
        <X :size="14" />
      </button>
    </div>

    <!-- ── Table ────────────────────────────────────────────────────────── -->
    <div class="card p-0 overflow-hidden">
      <div class="bg-[#0f0f0f] px-4 py-2 text-xs text-[rgba(245,245,220,0.40)] font-semibold uppercase tracking-wider border-b border-[#dc143c]/20">
        Participants
      </div>

      <table class="w-full text-sm">
        <thead class="bg-[#0f0f0f]">
          <tr class="text-[rgba(245,245,220,0.50)] text-left border-b border-[#dc143c]/20">
            <th class="px-4 py-3">Kind</th>
            <th class="px-4 py-3">Name</th>
            <th class="px-4 py-3">Identified by</th>
            <th class="px-4 py-3 text-right">Events</th>
            <th class="px-4 py-3 text-right">Tasks</th>
            <th class="px-4 py-3 text-right">Samples</th>
            <th class="px-4 py-3">Last seen</th>
          </tr>
        </thead>
        <tbody class="divide-y divide-[#1a1a1a]">
          <tr v-if="ugStore.taskLoading && !ugStore.actorList.length">
            <td colspan="7" class="px-4 py-6 text-center text-[rgba(245,245,220,0.40)]">Loading…</td>
          </tr>
          <tr v-else-if="!ugStore.actorList.length">
            <td colspan="7" class="px-4 py-8 text-center text-[rgba(245,245,220,0.30)] text-sm">
              No actors
              <span class="block mt-1 text-[rgba(245,245,220,0.20)] text-xs">
                Actor extraction is off by default — set ACTOR_NODES_ENABLED=true.
                Note it reads event attributes, so agents named only on lines that
                never become events are not captured.
              </span>
            </td>
          </tr>
          <tr
            v-for="a in ugStore.actorList"
            :key="a.actor_id"
            class="hover:bg-[#dc143c]/5 transition-colors"
          >
            <td class="px-4 py-2"><span :class="kindClass(a.kind)">{{ a.kind }}</span></td>
            <td class="px-4 py-2 font-mono text-[#f5f5dc] text-xs">{{ a.name }}</td>
            <td class="px-4 py-2 font-mono text-[rgba(245,245,220,0.40)] text-xs">{{ a.source_field }}</td>
            <td class="px-4 py-2 text-right text-[rgba(245,245,220,0.70)]">{{ a.event_count }}</td>
            <td class="px-4 py-2 text-right text-[rgba(245,245,220,0.70)]">{{ a.task_ids.length }}</td>
            <td class="px-4 py-2 text-right text-[rgba(245,245,220,0.70)]">{{ a.sample_hashes.length }}</td>
            <td class="px-4 py-2 text-[rgba(245,245,220,0.40)] text-xs">{{ fmt(a.last_seen) }}</td>
          </tr>
        </tbody>
      </table>
    </div>

    <!-- ── Pagination ───────────────────────────────────────────────────── -->
    <div class="flex items-center justify-between text-sm text-[rgba(245,245,220,0.40)]">
      <span>Page {{ page }}</span>
      <div class="flex gap-2">
        <button
          class="btn-secondary py-1 disabled:opacity-40 inline-flex items-center gap-1"
          :disabled="page <= 1"
          @click="loadPage(page - 1)"
        >
          <ChevronLeft :size="14" /> Prev
        </button>
        <button
          class="btn-secondary py-1 disabled:opacity-40 inline-flex items-center gap-1"
          :disabled="page * limit >= ugStore.actorTotal"
          @click="loadPage(page + 1)"
        >
          Next <ChevronRight :size="14" />
        </button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { RefreshCw, ChevronLeft, ChevronRight, X } from 'lucide-vue-next'
import { useUpsidegateStore } from '../../stores/upsidegate'
import type { ActorKind } from '../../types'

const ugStore = useUpsidegateStore()

const kind   = ref<ActorKind | ''>('')
const taskId = ref('')
const page   = ref(1)
const limit  = 25

async function loadPage(p: number) {
  page.value = Math.max(1, p)
  await ugStore.fetchActors({
    kind: kind.value || undefined,
    task_id: taskId.value.trim() || undefined,
    limit,
    page: page.value,
  })
}

function fmt(ts: string) {
  try { return new Date(ts).toLocaleString() } catch { return ts }
}

function kindClass(k: ActorKind) {
  switch (k) {
    case 'agent':    return 'badge badge-blue'
    case 'skill':    return 'badge badge-green'
    case 'resource': return 'badge badge-yellow'
    default:         return 'badge badge-slate'
  }
}

onMounted(() => loadPage(1))
</script>
