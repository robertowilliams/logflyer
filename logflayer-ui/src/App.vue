<template>
  <div class="min-h-screen bg-slate-900 text-slate-100">
    <!-- Sidebar -->
    <aside class="fixed top-0 left-0 z-40 w-64 h-screen bg-slate-800 border-r border-slate-700">
      <div class="h-full px-3 py-4 overflow-y-auto flex flex-col">
        <!-- Logo -->
        <div class="flex items-center mb-8 px-2">
          <div class="text-xl font-bold text-primary-400">⚡ Logflayer</div>
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
              class="flex items-center gap-3 p-2 rounded-lg hover:bg-slate-700 transition-colors"
              active-class="bg-primary-600 hover:bg-primary-700"
            >
              <span class="text-lg">{{ link.icon }}</span>
              <span>{{ link.label }}</span>
            </router-link>
          </li>
        </ul>

        <!-- Stats footer -->
        <div class="mt-auto px-2 pt-4 border-t border-slate-700 text-xs text-slate-500 space-y-1">
          <div>Active targets: <span class="text-slate-300">{{ store.activeTargets.length }}</span></div>
          <div>Total targets: <span class="text-slate-300">{{ store.targets.length }}</span></div>
        </div>
      </div>
    </aside>

    <!-- Main content -->
    <div class="ml-64">
      <header class="bg-slate-800 border-b border-slate-700 px-6 py-4 flex items-center justify-between">
        <h1 class="text-xl font-semibold">{{ currentTitle }}</h1>
        <button @click="store.checkHealth()" class="text-slate-400 hover:text-white text-sm transition-colors">
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
