<template>
  <div class="min-h-screen bg-[#0a0a0a] text-[#f5f5dc]">
    <!-- Sidebar -->
    <aside class="fixed top-0 left-0 z-40 w-64 h-screen bg-[#0f0f0f] border-r border-[#dc143c]/20">
      <div class="h-full px-3 py-4 overflow-y-auto flex flex-col">
        <!-- Logo -->
        <div class="flex items-center mb-8 px-2">
          <div class="text-xl font-bold bg-gradient-to-r from-[#dc143c] to-[#00d4ff] bg-clip-text text-transparent">
            ⚡ Logflayer
          </div>
        </div>

        <!-- Health indicator -->
        <div class="mb-6 px-2">
          <span :class="isHealthy ? 'badge-green' : 'badge-red'">
            {{ isHealthy ? '● Connected' : '● Disconnected' }}
          </span>
        </div>

        <!-- Navigation -->
        <ul class="space-y-1 font-medium flex-1">
          <li v-for="link in navLinks" :key="link.to">
            <router-link
              :to="link.to"
              class="flex items-center gap-3 p-2 rounded-lg hover:bg-[#dc143c]/10 text-[rgba(245,245,220,0.70)] hover:text-[#f5f5dc] transition-all duration-200"
              active-class="bg-[#dc143c]/20 text-[#f5f5dc] border-l-2 border-[#dc143c] shadow-[0_0_8px_rgba(220,20,60,0.2)]"
            >
              <span class="text-lg">{{ link.icon }}</span>
              <span>{{ link.label }}</span>
            </router-link>
          </li>
        </ul>

        <!-- Stats footer -->
        <div class="mt-auto px-2 pt-4 border-t border-[#dc143c]/20 text-xs text-[rgba(245,245,220,0.40)] space-y-1">
          <div>Active targets: <span class="text-[#00d4ff]">{{ store.activeTargets.length }}</span></div>
          <div>Total targets: <span class="text-[rgba(245,245,220,0.70)]">{{ store.targets.length }}</span></div>
        </div>
      </div>
    </aside>

    <!-- Main content -->
    <div class="ml-64">
      <header class="bg-[#0f0f0f] border-b border-[#dc143c]/20 px-6 py-4 flex items-center justify-between">
        <h1 class="text-xl font-semibold text-[#f5f5dc]">{{ currentTitle }}</h1>
        <button @click="store.checkHealth()" class="text-[rgba(245,245,220,0.40)] hover:text-[#00d4ff] text-sm transition-colors duration-200">
          ↻ Refresh
        </button>
      </header>
      <main class="p-6">
        <router-view />
      </main>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted } from 'vue'
import { useRoute } from 'vue-router'
import { useLogflayerStore } from './stores/logflayer'

const route = useRoute()
const store = useLogflayerStore()

const isHealthy = computed(() => store.isHealthy)
const currentTitle = computed(() => (route.meta.title as string) || 'Logflayer')

const navLinks = [
  { to: '/',         icon: '📊', label: 'Dashboard'       },
  { to: '/targets',  icon: '🎯', label: 'Targets'         },
  { to: '/logs',     icon: '📋', label: 'Live Logs'       },
  { to: '/tracking', icon: '🔍', label: 'Logging Tracker' },
  { to: '/samples',  icon: '🗄️',  label: 'Samples'        },
]

onMounted(async () => {
  await store.checkHealth()
  await store.fetchTargets()
})
</script>
