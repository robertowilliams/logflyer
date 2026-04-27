<template>
  <div class="space-y-6">
    <!-- Stat cards -->
    <div class="grid grid-cols-2 lg:grid-cols-4 gap-4">
      <div class="card text-center">
        <div class="text-3xl font-bold text-[#dc143c]">{{ store.targets.length }}</div>
        <div class="text-[rgba(245,245,220,0.50)] text-sm mt-1">Total Targets</div>
      </div>
      <div class="card text-center">
        <div class="text-3xl font-bold text-green-400">{{ store.activeTargets.length }}</div>
        <div class="text-[rgba(245,245,220,0.50)] text-sm mt-1">Active</div>
      </div>
      <div class="card text-center">
        <div class="text-3xl font-bold text-yellow-400">{{ store.inactiveTargets.length }}</div>
        <div class="text-[rgba(245,245,220,0.50)] text-sm mt-1">Inactive</div>
      </div>
      <div class="card text-center">
        <div class="text-3xl font-bold" :class="store.isHealthy ? 'text-green-400' : 'text-[#ff6b8a]'">
          {{ store.isHealthy ? '●' : '●' }}
        </div>
        <div class="text-[rgba(245,245,220,0.50)] text-sm mt-1">API {{ store.isHealthy ? 'Healthy' : 'Down' }}</div>
      </div>
    </div>

    <!-- Recent targets -->
    <div class="card">
      <div class="flex items-center justify-between mb-4">
        <h2 class="text-lg font-semibold text-[#f5f5dc]">Recent Targets</h2>
        <router-link to="/targets" class="text-[#00d4ff] hover:text-[#00b8d9] text-sm transition-colors">View all →</router-link>
      </div>
      <div v-if="store.loading" class="text-[rgba(245,245,220,0.50)] text-sm">Loading...</div>
      <div v-else-if="store.targets.length === 0" class="text-[rgba(245,245,220,0.40)] text-sm">No targets configured yet.</div>
      <table v-else class="w-full text-sm">
        <thead>
          <tr class="text-[rgba(245,245,220,0.50)] text-left border-b border-[#dc143c]/20">
            <th class="pb-2">Target ID</th>
            <th class="pb-2">Host</th>
            <th class="pb-2">Status</th>
            <th class="pb-2">Paths</th>
          </tr>
        </thead>
        <tbody class="divide-y divide-[#1a1a1a]">
          <tr v-for="t in recentTargets" :key="t.id" class="py-2 hover:bg-[#dc143c]/5 transition-colors">
            <td class="py-2 font-mono text-[#dc143c]">{{ t.target_id }}</td>
            <td class="py-2 text-[rgba(245,245,220,0.80)]">{{ t.host || t.hostname || t.server || '—' }}</td>
            <td class="py-2">
              <span :class="t.status === 'active' ? 'badge-green' : 'badge-red'">
                {{ t.status }}
              </span>
            </td>
            <td class="py-2 text-[rgba(245,245,220,0.50)]">{{ (t.log_paths || t.log_dirs || []).length }} path(s)</td>
          </tr>
        </tbody>
      </table>
    </div>

    <!-- Quick links -->
    <div class="grid grid-cols-1 md:grid-cols-3 gap-4">
      <router-link to="/logs" class="card hover:border-[#dc143c]/60 hover:shadow-[0_0_16px_rgba(220,20,60,0.15)] transition-all cursor-pointer block">
        <div class="text-2xl mb-2">📋</div>
        <div class="font-semibold text-[#f5f5dc]">Live Logs</div>
        <div class="text-[rgba(245,245,220,0.50)] text-sm mt-1">View the logflayer service log in real time</div>
      </router-link>
      <router-link to="/tracking" class="card hover:border-[#dc143c]/60 hover:shadow-[0_0_16px_rgba(220,20,60,0.15)] transition-all cursor-pointer block">
        <div class="text-2xl mb-2">🔍</div>
        <div class="font-semibold text-[#f5f5dc]">Logging Tracker</div>
        <div class="text-[rgba(245,245,220,0.50)] text-sm mt-1">Browse records in loggingtracker.logging_tracks</div>
      </router-link>
      <router-link to="/samples" class="card hover:border-[#dc143c]/60 hover:shadow-[0_0_16px_rgba(220,20,60,0.15)] transition-all cursor-pointer block">
        <div class="text-2xl mb-2">🗄️</div>
        <div class="font-semibold text-[#f5f5dc]">Samples</div>
        <div class="text-[rgba(245,245,220,0.50)] text-sm mt-1">Explore sampled log records from all targets</div>
      </router-link>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { useLogflayerStore } from '../stores/logflayer'

const store = useLogflayerStore()
const recentTargets = computed(() => store.targets.slice(0, 8))
</script>
