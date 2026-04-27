<template>
  <div class="space-y-6">
    <!-- Toolbar -->
    <div class="flex items-center justify-between">
      <div class="flex gap-2">
        <input v-model="search" type="text" placeholder="Search targets…" class="input w-64" />
        <select v-model="statusFilter" class="input w-36">
          <option value="">All</option>
          <option value="active">Active</option>
          <option value="inactive">Inactive</option>
        </select>
      </div>
      <button @click="openCreate" class="btn-primary">+ New Target</button>
    </div>

    <!-- Table -->
    <div class="card p-0 overflow-hidden">
      <table class="w-full text-sm">
        <thead class="bg-[#0f0f0f]">
          <tr class="text-[rgba(245,245,220,0.50)] text-left border-b border-[#dc143c]/20">
            <th class="px-4 py-3">Target ID</th>
            <th class="px-4 py-3">Host</th>
            <th class="px-4 py-3">Port</th>
            <th class="px-4 py-3">User</th>
            <th class="px-4 py-3">Auth</th>
            <th class="px-4 py-3">Paths</th>
            <th class="px-4 py-3">Status</th>
            <th class="px-4 py-3">Actions</th>
          </tr>
        </thead>
        <tbody class="divide-y divide-[#1a1a1a]">
          <tr v-if="store.loading">
            <td colspan="8" class="px-4 py-6 text-center text-[rgba(245,245,220,0.40)]">Loading…</td>
          </tr>
          <tr v-else-if="filtered.length === 0">
            <td colspan="8" class="px-4 py-6 text-center text-[rgba(245,245,220,0.30)]">No targets found.</td>
          </tr>
          <tr v-for="t in filtered" :key="t.id" class="hover:bg-[#dc143c]/5 transition-colors">
            <td class="px-4 py-3 font-mono text-[#dc143c]">{{ t.target_id }}</td>
            <td class="px-4 py-3 text-[rgba(245,245,220,0.80)]">{{ t.host || t.hostname || t.server || '—' }}</td>
            <td class="px-4 py-3 text-[rgba(245,245,220,0.50)]">{{ t.port || 22 }}</td>
            <td class="px-4 py-3 text-[rgba(245,245,220,0.50)]">{{ t.username || t.user || '—' }}</td>
            <td class="px-4 py-3">
              <span class="badge-blue">{{ authLabel(t) }}</span>
            </td>
            <td class="px-4 py-3 text-[rgba(245,245,220,0.50)]">{{ (t.log_paths || t.log_dirs || []).length }}</td>
            <td class="px-4 py-3">
              <span :class="t.status === 'active' ? 'badge-green' : 'badge-red'">{{ t.status }}</span>
            </td>
            <td class="px-4 py-3">
              <div class="flex gap-2">
                <button @click="openEdit(t)" class="text-[#00d4ff] hover:text-[#00b8d9] text-xs transition-colors">Edit</button>
                <button @click="toggle(t.id)" class="text-yellow-400 hover:text-yellow-300 text-xs transition-colors">Toggle</button>
                <button @click="remove(t.id)" class="text-[#ff6b8a] hover:text-[#dc143c] text-xs transition-colors">Delete</button>
              </div>
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <!-- Modal -->
    <div v-if="showModal" class="fixed inset-0 bg-black/70 flex items-center justify-center z-50 p-4 backdrop-blur-sm">
      <div class="bg-[#141414] border border-[#dc143c]/30 rounded-xl w-full max-w-2xl max-h-[90vh] overflow-y-auto shadow-[0_0_40px_rgba(220,20,60,0.2)]">
        <div class="p-6">
          <h2 class="text-lg font-semibold mb-6 text-[#f5f5dc]">{{ editingTarget ? 'Edit Target' : 'New Target' }}</h2>
          <TargetForm :initial="editingTarget" @save="onSave" @cancel="closeModal" />
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useLogflayerStore } from '../stores/logflayer'
import TargetForm from '../components/TargetForm.vue'
import type { Target } from '../types'

const store = useLogflayerStore()
const search = ref('')
const statusFilter = ref('')
const showModal = ref(false)
const editingTarget = ref<Target | null>(null)

const filtered = computed(() =>
  store.targets.filter(t => {
    const matchSearch = !search.value ||
      t.target_id?.toLowerCase().includes(search.value.toLowerCase()) ||
      (t.host || t.hostname || '').toLowerCase().includes(search.value.toLowerCase())
    const matchStatus = !statusFilter.value || t.status === statusFilter.value
    return matchSearch && matchStatus
  })
)

function authLabel(t: Target) {
  if (t.private_key || t.credentials?.private_key) return 'key-inline'
  if (t.private_key_path || t.credentials?.private_key_path) return 'key-file'
  if (t.password || t.credentials?.password) return 'password'
  return 'agent'
}

function openCreate() { editingTarget.value = null; showModal.value = true }
function openEdit(t: Target) { editingTarget.value = { ...t }; showModal.value = true }
function closeModal() { showModal.value = false; editingTarget.value = null }

async function onSave(data: Partial<Target>) {
  if (editingTarget.value?.id) {
    await store.updateTarget(editingTarget.value.id, data)
  } else {
    await store.createTarget(data)
  }
  closeModal()
}

async function toggle(id: string) { await store.toggleTarget(id) }

async function remove(id: string) {
  if (confirm('Delete this target?')) await store.deleteTarget(id)
}

onMounted(() => store.fetchTargets())
</script>
