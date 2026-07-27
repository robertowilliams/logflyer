<template>
  <!-- Detached / chrome-less windows render the view fullscreen with no sidebar. -->
  <router-view v-if="isBare" />
  <div v-else class="min-h-screen bg-[#0a0a0a] text-[#f5f5dc]">
    <!-- Sidebar -->
    <aside class="fixed top-0 left-0 z-40 w-64 h-screen bg-[#0f0f0f] border-r border-[#dc143c]/20">
      <div class="h-full px-3 py-4 overflow-y-auto flex flex-col">
        <!-- Logo -->
        <div class="mb-8 px-2 flex flex-col gap-2">
          <img :src="vectaLogoUrl" alt="VectaDB" class="h-6 w-auto" />
          <img :src="circlesLogoUrl" alt="VectaDB" class="h-5 w-auto" />
        </div>

        <!-- Health indicator -->
        <div class="mb-6 px-2">
          <span :class="isHealthy ? 'badge-green' : 'badge-red'" class="inline-flex items-center gap-1.5">
            <span class="w-1.5 h-1.5 rounded-full bg-current" />
            {{ isHealthy ? 'Connected' : 'Disconnected' }}
          </span>
        </div>

        <!-- Navigation -->
        <div class="flex-1 space-y-4">
          <!-- Logflayer section -->
          <ul class="space-y-1 font-medium">
            <li v-for="link in navLinks" :key="link.to">
              <router-link
                :to="link.to"
                class="flex items-center gap-3 p-2 rounded-lg hover:bg-[#dc143c]/10 text-[rgba(245,245,220,0.70)] hover:text-[#f5f5dc] transition-all duration-200"
                active-class="bg-[#dc143c]/20 text-[#f5f5dc] border-l-2 border-[#dc143c] shadow-[0_0_8px_rgba(220,20,60,0.2)]"
              >
                <component :is="link.icon" :size="18" class="shrink-0" />
                <span>{{ link.label }}</span>
              </router-link>
            </li>
          </ul>

          <!-- UpsideGate section -->
          <div>
            <div class="px-2 py-1 text-[10px] font-semibold uppercase tracking-widest text-[rgba(245,245,220,0.25)]">
              UpsideGate ETL
            </div>
            <ul class="space-y-1 font-medium mt-1">
              <li v-for="link in ugNavLinks" :key="link.to">
                <router-link
                  :to="link.to"
                  class="flex items-center gap-3 p-2 rounded-lg hover:bg-[#00d4ff]/10 text-[rgba(245,245,220,0.60)] hover:text-[#f5f5dc] transition-all duration-200"
                  active-class="bg-[#00d4ff]/15 text-[#f5f5dc] border-l-2 border-[#00d4ff] shadow-[0_0_8px_rgba(0,212,255,0.1)]"
                >
                  <component :is="link.icon" :size="18" class="shrink-0" />
                  <span>{{ link.label }}</span>
                </router-link>
              </li>
            </ul>
          </div>
        </div>

        <!-- Footer: module identity + stats -->
        <div class="mt-auto px-2 pt-4 border-t border-[#dc143c]/20 space-y-3">
          <div>
            <div class="text-[9px] uppercase tracking-widest text-[rgba(245,245,220,0.25)] mb-1">Module</div>
            <img :src="logflayerLogoUrl" alt="LogFlayer" class="h-4 w-auto opacity-80" />
          </div>
          <div class="text-xs text-[rgba(245,245,220,0.40)] space-y-1">
            <div>Active targets: <span class="text-[#00d4ff]">{{ store.activeTargets.length }}</span></div>
            <div>Total targets: <span class="text-[rgba(245,245,220,0.70)]">{{ store.targets.length }}</span></div>
          </div>
        </div>
      </div>
    </aside>

    <!-- Main content -->
    <div class="ml-64">
      <header class="bg-[#0f0f0f] border-b border-[#dc143c]/20 px-6 py-4 flex items-center justify-between">
        <h1 class="text-xl font-semibold text-[#f5f5dc]">{{ currentTitle }}</h1>
        <button @click="store.checkHealth()" class="inline-flex items-center gap-1.5 text-[rgba(245,245,220,0.40)] hover:text-[#00d4ff] text-sm transition-colors duration-200">
          <RefreshCw :size="15" />
          Refresh
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
import {
  RefreshCw, LayoutDashboard, Target, ScrollText, Radar, Database,
  Brain, Trash2, Settings, Microscope, Share2, Ruler, Radio,
  ClipboardList, Users,
} from 'lucide-vue-next'
import { useLogflayerStore } from './stores/logflayer'
import vectaLogoUrl from './assets/vectadb-logo.svg'
import circlesLogoUrl from './assets/circles-logo.svg'
import logflayerLogoUrl from './assets/logflayer-logo.svg'

const route = useRoute()
const store = useLogflayerStore()

const isHealthy = computed(() => store.isHealthy)
const isBare = computed(() => route.meta.bare === true)
const currentTitle = computed(() => (route.meta.title as string) || 'Logflayer')

const navLinks = [
  { to: '/',                icon: LayoutDashboard, label: 'Dashboard'       },
  { to: '/targets',         icon: Target,          label: 'Targets'         },
  { to: '/logs',            icon: ScrollText,      label: 'Live Logs'       },
  { to: '/tracking',        icon: Radar,           label: 'Logging Tracker' },
  { to: '/samples',         icon: Database,        label: 'Samples'         },
  { to: '/classifications', icon: Brain,           label: 'Classifications' },
  { to: '/deletions',       icon: Trash2,          label: 'Deletions'       },
  { to: '/admin',           icon: Settings,        label: 'Admin Settings'  },
]

const ugNavLinks = [
  { to: '/upsidegate/tasks',     icon: ClipboardList, label: 'Task Audit'    },
  { to: '/upsidegate/actors',    icon: Users,         label: 'Agents & Skills' },
  { to: '/upsidegate/entities',  icon: Microscope, label: 'Entity Browser' },
  { to: '/upsidegate/relations', icon: Share2,     label: 'Relation Graph' },
  { to: '/upsidegate/prov',      icon: Ruler,      label: 'PROV-O Triples' },
  { to: '/upsidegate/spans',     icon: Radio,      label: 'OTel Spans'     },
]

onMounted(async () => {
  await store.checkHealth()
  await store.fetchTargets()
})
</script>
