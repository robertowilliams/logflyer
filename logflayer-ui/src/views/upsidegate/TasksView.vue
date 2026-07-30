<!--
  Task browser — the search-then-audit loop (Stages 11–13).

  A task is a unit of work correlated on a key found in the log itself, so it
  spans however many samples it was collected in. This view walks the loop the
  stages were built for:

    browse tasks → read what one was *for* → find semantically similar tasks
                 → open the interaction graph and audit the reasoning

  `task_id_source` is shown on every row deliberately. A boundary derived from
  `session_id` means something; one derived from the sample hash only means "we
  could not tell", and presenting the two with equal confidence would be the
  whole point of the audit trail thrown away.
-->
<template>
  <div class="space-y-4">
    <!-- ── Filters ──────────────────────────────────────────────────────── -->
    <div class="flex flex-wrap gap-3 items-center">
      <select v-model="targetId" class="input w-44" @change="loadPage(1)">
        <option value="">All targets</option>
        <option v-for="t in store.sampleCollections" :key="t" :value="t">{{ t }}</option>
      </select>

      <select
        v-model="ugStore.filterTaskStatus"
        class="input w-36"
        title="What the log shows about whether the work is still running, finished, or failed"
        @change="loadPage(1)"
      >
        <option value="">All statuses</option>
        <option value="running">Running</option>
        <option value="completed">Completed</option>
        <option value="failed">Failed</option>
      </select>

      <button
        :class="[
          'px-3 py-1 rounded-full text-xs font-mono border transition-all',
          ugStore.realBoundariesOnly
            ? 'bg-[#dc143c]/20 border-[#dc143c]/60 text-[#f5f5dc]'
            : 'bg-[#1a1a1a] border-[#333] text-[rgba(245,245,220,0.60)] hover:border-[#dc143c]/40',
        ]"
        title="Hide tasks that fell back to sample scope — those are not task boundaries, just placeholders for logs carrying no correlation key"
        @click="toggleBoundaries"
      >
        real boundaries only
      </button>

      <button class="btn-primary inline-flex items-center gap-1.5" @click="loadPage(page)">
        <RefreshCw :size="14" /> Refresh
      </button>

      <span class="ml-auto text-[rgba(245,245,220,0.40)] text-sm self-center">
        {{ ugStore.taskTotal }} task{{ ugStore.taskTotal === 1 ? '' : 's' }}
      </span>
    </div>

    <!-- ── Error banner ─────────────────────────────────────────────────── -->
    <div
      v-if="ugStore.taskError && !ugStore.searchUnavailable"
      class="card border-[#dc143c]/60 bg-[#dc143c]/10 text-[#ff6b8a] text-sm flex items-center justify-between"
    >
      <span>{{ ugStore.taskError }}</span>
      <button class="text-[#ff6b8a] hover:text-[#f5f5dc]" @click="ugStore.clearTaskError()">
        <X :size="14" />
      </button>
    </div>

    <!-- ── Task table ───────────────────────────────────────────────────── -->
    <div class="card p-0 overflow-hidden">
      <div class="bg-[#0f0f0f] px-4 py-2 text-xs text-[rgba(245,245,220,0.40)] font-semibold uppercase tracking-wider border-b border-[#dc143c]/20">
        Tasks
      </div>

      <table class="w-full text-sm">
        <thead class="bg-[#0f0f0f]">
          <tr class="text-[rgba(245,245,220,0.50)] text-left border-b border-[#dc143c]/20">
            <th class="px-4 py-3">Task</th>
            <th class="px-4 py-3">Status</th>
            <th class="px-4 py-3">Boundary</th>
            <th class="px-4 py-3">Intent</th>
            <th class="px-4 py-3 text-right">Samples</th>
            <th class="px-4 py-3 text-right">Events</th>
            <th class="px-4 py-3 text-right">Edges</th>
            <th class="px-4 py-3">Last seen</th>
          </tr>
        </thead>
        <tbody class="divide-y divide-[#1a1a1a]">
          <tr v-if="ugStore.taskLoading && !ugStore.taskList.length">
            <td colspan="8" class="px-4 py-6 text-center text-[rgba(245,245,220,0.40)]">Loading…</td>
          </tr>
          <tr v-else-if="!ugStore.taskList.length">
            <td colspan="8" class="px-4 py-8 text-center text-[rgba(245,245,220,0.30)] text-sm">
              No tasks
              <span class="block mt-1 text-[rgba(245,245,220,0.20)] text-xs">
                Task correlation is off by default — set TASK_CORRELATION_ENABLED=true
                and ingest a sample whose log carries a correlation key.
              </span>
            </td>
          </tr>
          <tr
            v-for="t in ugStore.taskList"
            :key="t.task_id"
            class="hover:bg-[#dc143c]/5 transition-colors cursor-pointer"
            :class="ugStore.selectedTask?.task_id === t.task_id ? 'bg-[#dc143c]/10 border-l-2 border-[#dc143c]' : ''"
            @click="pick(t)"
          >
            <td class="px-4 py-2 font-mono text-[#00d4ff] text-xs">{{ t.task_id.slice(0, 12) }}</td>
            <td class="px-4 py-2">
              <span :class="statusClass(t.status)" :title="statusTitle(t.status)">{{ t.status ?? 'running' }}</span>
            </td>
            <td class="px-4 py-2">
              <span :class="boundaryClass(t.task_id_source)">{{ t.task_id_source }}</span>
              <span
                v-if="t.correlation_key"
                class="ml-2 font-mono text-xs text-[rgba(245,245,220,0.40)]"
              >{{ t.correlation_key }}</span>
            </td>
            <td class="px-4 py-2 text-[rgba(245,245,220,0.70)] text-xs">
              <span v-if="t.intent_text">{{ truncate(t.intent_text, 60) }}</span>
              <span v-else class="text-[rgba(245,245,220,0.25)] italic">no stated goal</span>
            </td>
            <td class="px-4 py-2 text-right text-[rgba(245,245,220,0.70)]">{{ t.sample_hashes.length }}</td>
            <td class="px-4 py-2 text-right text-[rgba(245,245,220,0.70)]">{{ t.entity_count }}</td>
            <td class="px-4 py-2 text-right text-[rgba(245,245,220,0.70)]">{{ t.relation_count }}</td>
            <td class="px-4 py-2 text-[rgba(245,245,220,0.40)] text-xs">{{ fmt(t.last_seen) }}</td>
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
          :disabled="page * limit >= ugStore.taskTotal"
          @click="loadPage(page + 1)"
        >
          Next <ChevronRight :size="14" />
        </button>
      </div>
    </div>

    <!-- ── Selected task ────────────────────────────────────────────────── -->
    <template v-if="ugStore.selectedTask">
      <div class="card bg-[#0a0a0a] border-[#00d4ff]/20">
        <div class="flex items-center justify-between mb-3">
          <h3 class="text-sm font-semibold text-[#f5f5dc]">What this task was for</h3>
          <button
            class="text-[rgba(245,245,220,0.40)] hover:text-[#f5f5dc] transition-colors text-xs inline-flex items-center gap-1"
            @click="ugStore.selectTask(null)"
          >
            <X :size="12" /> Close
          </button>
        </div>

        <p
          v-if="ugStore.selectedTask.intent_text"
          class="text-sm text-[#f5f5dc] leading-relaxed"
        >{{ ugStore.selectedTask.intent_text }}</p>
        <p v-else class="text-sm text-[rgba(245,245,220,0.30)] italic">
          No goal statement was found in this task's logs. Intent is only extracted
          from a system prompt, a user turn, or reasoning carrying an explicit
          marker — lifecycle chatter is deliberately not treated as a goal.
        </p>

        <!-- Coarser-than-ideal boundary warning -->
        <div
          v-if="ugStore.selectedTask.task_id_source === 'sample'"
          class="mt-3 card border-[#f59e0b]/50 bg-[#f59e0b]/10 text-[#f59e0b] text-xs"
        >
          This boundary is the sample fallback — the log carried no correlation key,
          so "task" here means "one sample". It is not evidence that these events
          belong together.
        </div>

        <div class="grid grid-cols-2 gap-x-8 gap-y-2 font-mono text-xs mt-4">
          <div>
            <span class="text-[rgba(245,245,220,0.40)]">task_id</span><br>
            <span class="text-[#f5f5dc]">{{ ugStore.selectedTask.task_id }}</span>
          </div>
          <div>
            <span class="text-[rgba(245,245,220,0.40)]">status</span><br>
            <span :class="statusClass(ugStore.selectedTask.status)">{{ ugStore.selectedTask.status ?? 'running' }}</span>
          </div>
          <div>
            <span class="text-[rgba(245,245,220,0.40)]">boundary</span><br>
            <span class="text-[#f5f5dc]">
              {{ ugStore.selectedTask.task_id_source }}
              <template v-if="ugStore.selectedTask.correlation_key">
                = {{ ugStore.selectedTask.correlation_key }}
              </template>
            </span>
          </div>
          <div>
            <span class="text-[rgba(245,245,220,0.40)]">targets</span><br>
            <span class="text-[#dc143c]">{{ ugStore.selectedTask.target_ids.join(', ') || '—' }}</span>
          </div>
          <div>
            <span class="text-[rgba(245,245,220,0.40)]">first seen</span><br>
            <span class="text-[#f5f5dc]">{{ fmt(ugStore.selectedTask.first_seen) }}</span>
          </div>
        </div>
      </div>

      <!-- ── Semantically similar tasks ─────────────────────────────────── -->
      <div class="card p-0 overflow-hidden">
        <div class="bg-[#0f0f0f] px-4 py-2 flex items-center justify-between border-b border-[#dc143c]/20">
          <span class="text-xs text-[rgba(245,245,220,0.40)] font-semibold uppercase tracking-wider">
            Similar tasks
          </span>
          <button
            class="btn-secondary py-1 text-xs inline-flex items-center gap-1 disabled:opacity-40"
            :disabled="ugStore.searching"
            @click="ugStore.findSimilarTasks(ugStore.selectedTask!.task_id)"
          >
            <Search :size="12" />
            {{ ugStore.searching ? 'Searching…' : 'Find similar' }}
          </button>
        </div>

        <!-- Not an error: intents are only embedded when a provider is configured. -->
        <div
          v-if="ugStore.searchUnavailable"
          class="px-4 py-4 text-xs text-[#f59e0b] bg-[#f59e0b]/5"
        >
          {{ ugStore.searchError }}
          <span class="block mt-1 text-[rgba(245,245,220,0.40)]">
            Intents are embedded by the embedding worker, which needs an API key.
            Without one the graph below is still complete — only the semantic
            lookup is unavailable.
          </span>
        </div>
        <div
          v-else-if="ugStore.searchError"
          class="px-4 py-4 text-xs text-[#ff6b8a] bg-[#dc143c]/5"
        >{{ ugStore.searchError }}</div>

        <table v-else-if="ugStore.similarTasks.length" class="w-full text-xs">
          <thead class="bg-[#0f0f0f]">
            <tr class="text-[rgba(245,245,220,0.50)] text-left border-b border-[#dc143c]/20">
              <th class="px-3 py-2">Task</th>
              <th class="px-3 py-2 text-right">Similarity</th>
              <th class="px-3 py-2">Model</th>
            </tr>
          </thead>
          <tbody class="divide-y divide-[#1a1a1a]">
            <tr
              v-for="h in ugStore.similarTasks"
              :key="h.embedding_id"
              class="hover:bg-[#dc143c]/5 transition-colors"
              :class="h.task_id ? 'cursor-pointer' : ''"
              @click="h.task_id && openTaskById(h.task_id)"
            >
              <td class="px-3 py-2 font-mono text-[#00d4ff]">{{ (h.task_id ?? h.sample_hash).slice(0, 12) }}</td>
              <td class="px-3 py-2 text-right text-[rgba(245,245,220,0.70)]">{{ h.score.toFixed(4) }}</td>
              <td class="px-3 py-2 text-[rgba(245,245,220,0.40)]">{{ h.model }}</td>
            </tr>
          </tbody>
        </table>

        <div v-else class="px-4 py-6 text-center text-[rgba(245,245,220,0.30)] text-sm">
          Search by what this task was for, to find others that attempted
          something similar.
        </div>
      </div>

      <!-- ── The audit payload ──────────────────────────────────────────── -->
      <div class="card">
        <div class="flex items-center justify-between mb-3">
          <h3 class="text-sm font-semibold text-[#f5f5dc]">Interaction graph</h3>
          <span v-if="ugStore.taskGraph" class="text-xs text-[rgba(245,245,220,0.40)]">
            {{ ugStore.taskGraph.entity_count }} events ·
            {{ ugStore.taskGraph.actor_count }} participants ·
            {{ ugStore.taskGraphRelations.length }} edges ·
            {{ ugStore.taskGraph.sample_count }} sample{{ ugStore.taskGraph.sample_count === 1 ? '' : 's' }}
            <!--
              Say what was left out rather than quietly showing a smaller number
              than the task record reports.
            -->
            <template v-if="ugStore.taskGraphStructuralCount">
              ·
              <span
                class="text-[rgba(245,245,220,0.25)]"
                title="PART_OF edges point at the OTel trace id, which is neither an event nor a participant. The task is already the grouping, so they are hidden here — the API still returns them."
              >{{ ugStore.taskGraphStructuralCount }} trace-grouping edges hidden</span>
            </template>
          </span>
        </div>

        <!-- Truncation is a claim about completeness, so it must be visible. -->
        <div
          v-if="ugStore.taskGraph?.truncated"
          class="mb-3 card border-[#f59e0b]/50 bg-[#f59e0b]/10 text-[#f59e0b] text-xs"
        >
          This task spans more samples than one response assembles. What you see
          is the earliest {{ ugStore.taskGraph.sample_count }} — treat it as a
          window, not the whole task.
        </div>

        <div v-if="ugStore.taskLoading" class="px-4 py-6 text-center text-[rgba(245,245,220,0.40)] text-sm">
          Loading…
        </div>
        <RelationGraph
          v-else-if="ugStore.taskGraphRelations.length"
          :relations="ugStore.taskGraphRelations"
          :entities="ugStore.taskGraphNodes"
        />
        <div v-else class="text-center py-12 text-[rgba(245,245,220,0.30)]">
          <div class="flex justify-center mb-3">
            <Share2 :size="36" class="text-[rgba(245,245,220,0.30)]" />
          </div>
          <div class="text-sm">
            No edges for this task
            <span class="block mt-1 text-xs text-[rgba(245,245,220,0.20)]">
              Relation extraction needs the graph writer enabled, and a single
              isolated event has nothing to connect to.
            </span>
          </div>
        </div>
      </div>

      <!-- ── Participants ───────────────────────────────────────────────── -->
      <div v-if="ugStore.taskGraph?.actors.length" class="card p-0 overflow-hidden">
        <div class="bg-[#0f0f0f] px-4 py-2 text-xs text-[rgba(245,245,220,0.40)] font-semibold uppercase tracking-wider border-b border-[#dc143c]/20">
          Who and what took part
        </div>
        <table class="w-full text-xs">
          <thead class="bg-[#0f0f0f]">
            <tr class="text-[rgba(245,245,220,0.50)] text-left border-b border-[#dc143c]/20">
              <th class="px-3 py-2">Kind</th>
              <th class="px-3 py-2">Name</th>
              <th class="px-3 py-2">Identified by</th>
              <th class="px-3 py-2 text-right">Events</th>
            </tr>
          </thead>
          <tbody class="divide-y divide-[#1a1a1a]">
            <tr v-for="a in ugStore.taskGraph.actors" :key="a.actor_id">
              <td class="px-3 py-2"><span :class="kindClass(a.kind)">{{ a.kind }}</span></td>
              <td class="px-3 py-2 font-mono text-[#f5f5dc]">{{ a.name }}</td>
              <td class="px-3 py-2 text-[rgba(245,245,220,0.40)] font-mono">{{ a.source_field }}</td>
              <td class="px-3 py-2 text-right text-[rgba(245,245,220,0.70)]">{{ a.event_count }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </template>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { RefreshCw, ChevronLeft, ChevronRight, X, Search, Share2 } from 'lucide-vue-next'
import RelationGraph from '../../components/RelationGraph.vue'
import { client } from '../../api/client'
import { useLogflayerStore } from '../../stores/logflayer'
import { useUpsidegateStore } from '../../stores/upsidegate'
import type { TaskRecord, TaskStatus, ActorKind } from '../../types'

const store   = useLogflayerStore()
const ugStore = useUpsidegateStore()

const targetId = ref('')
const page  = ref(1)
const limit = 25

async function loadPage(p: number) {
  page.value = Math.max(1, p)
  await ugStore.fetchTasks({
    target_id: targetId.value || undefined,
    real_boundaries_only: ugStore.realBoundariesOnly,
    status: ugStore.filterTaskStatus || undefined,
    limit,
    page: page.value,
  })
}

function toggleBoundaries() {
  ugStore.realBoundariesOnly = !ugStore.realBoundariesOnly
  loadPage(1)
}

function pick(t: TaskRecord) {
  // Clicking the selected row closes it, matching the other UpsideGate views.
  if (ugStore.selectedTask?.task_id === t.task_id) {
    ugStore.selectTask(null)
  } else {
    ugStore.selectTask(t)
  }
}

/**
 * Open a task the search returned, which may not be on the current page.
 *
 * Falls back to fetching it directly rather than silently doing nothing — the
 * whole point of the search is to reach tasks you were not already looking at.
 */
async function openTaskById(taskId: string) {
  const onPage = ugStore.taskList.find(t => t.task_id === taskId)
  if (onPage) {
    await ugStore.selectTask(onPage)
    return
  }
  try {
    const { task } = await client.getTask(taskId)
    await ugStore.selectTask(task)
  } catch (e: any) {
    ugStore.taskError = e.response?.data?.error ?? e.message ?? 'Failed to open that task'
  }
}

function fmt(ts: string) {
  try { return new Date(ts).toLocaleString() } catch { return ts }
}

function truncate(s: string, n: number) {
  return s.length > n ? `${s.slice(0, n)}…` : s
}

/** The sample fallback is not a real boundary, so it must not look like one. */
function boundaryClass(source: string) {
  return source === 'sample' ? 'badge badge-slate' : 'badge badge-blue'
}

/**
 * Badge colour for a task's status (Stage 14).
 *
 * Missing `status` reads the same as `'running'` — a task written before
 * Stage 14 has no evidence it ended, which is exactly what `running` means —
 * so it gets the same badge rather than a distinct "unknown" one.
 */
function statusClass(status: TaskStatus | undefined) {
  switch (status ?? 'running') {
    case 'failed':    return 'badge badge-red'
    case 'completed': return 'badge badge-green'
    default:          return 'badge badge-blue'
  }
}

function statusTitle(status: TaskStatus | undefined) {
  switch (status ?? 'running') {
    case 'failed':    return 'A span in this task reported an error'
    case 'completed': return 'A terminal marker was seen and nothing errored'
    default:          return 'No evidence in the log that this task ended'
  }
}

function kindClass(kind: ActorKind) {
  switch (kind) {
    case 'agent':    return 'badge badge-blue'
    case 'skill':    return 'badge badge-green'
    case 'resource': return 'badge badge-yellow'
    default:         return 'badge badge-slate'
  }
}

onMounted(async () => {
  await store.fetchSampleCollections()
  await loadPage(1)
})
</script>
