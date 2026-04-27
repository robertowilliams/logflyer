<template>
  <form @submit.prevent="submit" class="space-y-4">
    <div class="grid grid-cols-2 gap-4">
      <div>
        <label class="block text-slate-400 text-xs mb-1">Target ID *</label>
        <input v-model="form.target_id" required class="input" placeholder="my-server-01" />
      </div>
      <div>
        <label class="block text-slate-400 text-xs mb-1">Status</label>
        <select v-model="form.status" class="input">
          <option value="active">active</option>
          <option value="inactive">inactive</option>
        </select>
      </div>
    </div>

    <div class="grid grid-cols-3 gap-4">
      <div class="col-span-2">
        <label class="block text-slate-400 text-xs mb-1">Host *</label>
        <input v-model="form.host" required class="input" placeholder="10.0.0.1 or hostname" />
      </div>
      <div>
        <label class="block text-slate-400 text-xs mb-1">SSH Port</label>
        <input v-model.number="form.port" type="number" class="input" placeholder="22" />
      </div>
    </div>

    <div>
      <label class="block text-slate-400 text-xs mb-1">Username *</label>
      <input v-model="form.username" required class="input" placeholder="ubuntu" />
    </div>

    <div>
      <label class="block text-slate-400 text-xs mb-1">Auth Method</label>
      <select v-model="authMethod" class="input">
        <option value="none">SSH Agent / None</option>
        <option value="password">Password</option>
        <option value="key_inline">Private Key (inline)</option>
        <option value="key_path">Private Key (file path)</option>
      </select>
    </div>

    <div v-if="authMethod === 'password'">
      <label class="block text-slate-400 text-xs mb-1">Password</label>
      <input v-model="form.password" type="password" class="input" placeholder="••••••••" />
    </div>

    <div v-if="authMethod === 'key_inline'" class="space-y-3">
      <div>
        <label class="block text-slate-400 text-xs mb-1">Private Key (PEM)</label>
        <textarea v-model="form.private_key" rows="6" class="input font-mono text-xs"
          placeholder="-----BEGIN OPENSSH PRIVATE KEY-----&#10;...&#10;-----END OPENSSH PRIVATE KEY-----" />
      </div>
      <div>
        <label class="block text-slate-400 text-xs mb-1">Passphrase (optional)</label>
        <input v-model="form.private_key_passphrase" type="password" class="input" placeholder="optional" />
      </div>
    </div>

    <div v-if="authMethod === 'key_path'" class="space-y-3">
      <div>
        <label class="block text-slate-400 text-xs mb-1">Key File Path (on logflayer host)</label>
        <input v-model="form.private_key_path" class="input font-mono" placeholder="/home/ubuntu/.ssh/id_rsa" />
      </div>
      <div>
        <label class="block text-slate-400 text-xs mb-1">Passphrase (optional)</label>
        <input v-model="form.private_key_passphrase" type="password" class="input" placeholder="optional" />
      </div>
    </div>

    <div>
      <label class="block text-slate-400 text-xs mb-1">Log Paths (one per line) *</label>
      <textarea v-model="logPathsText" rows="4" class="input font-mono text-sm"
        placeholder="/var/log/app&#10;/var/log/nginx" />
      <p class="text-slate-500 text-xs mt-1">Enter one directory path per line</p>
    </div>

    <div v-if="error" class="text-red-400 text-sm">{{ error }}</div>

    <div class="flex justify-end gap-3 pt-2">
      <button type="button" @click="$emit('cancel')" class="btn-secondary">Cancel</button>
      <button type="submit" class="btn-primary">{{ initial ? 'Save Changes' : 'Create Target' }}</button>
    </div>
  </form>
</template>

<script setup lang="ts">
import { ref, watch } from 'vue'
import type { Target } from '../types'

const props = defineProps<{ initial?: Target | null }>()
const emit = defineEmits<{ save: [data: Partial<Target>]; cancel: [] }>()

const form = ref<Partial<Target>>({ status: 'active', port: 22 })
const authMethod = ref<'none' | 'password' | 'key_inline' | 'key_path'>('none')
const logPathsText = ref('')
const error = ref('')

watch(() => props.initial, (t) => {
  if (!t) {
    form.value = { status: 'active', port: 22 }
    authMethod.value = 'none'
    logPathsText.value = ''
    return
  }
  form.value = { ...t }
  logPathsText.value = (t.log_paths || t.log_dirs || []).join('\n')
  if (t.private_key || (t.credentials as any)?.private_key) authMethod.value = 'key_inline'
  else if (t.private_key_path || (t.credentials as any)?.private_key_path) authMethod.value = 'key_path'
  else if (t.password || (t.credentials as any)?.password) authMethod.value = 'password'
  else authMethod.value = 'none'
}, { immediate: true })

function submit() {
  error.value = ''
  const paths = logPathsText.value.split('\n').map(s => s.trim()).filter(Boolean)
  if (paths.length === 0) { error.value = 'At least one log path is required.'; return }

  const payload: Partial<Target> = {
    target_id: form.value.target_id,
    status: form.value.status || 'active',
    host: form.value.host,
    port: form.value.port || 22,
    username: form.value.username,
    log_paths: paths,
  }

  if (authMethod.value === 'password') payload.password = form.value.password
  else if (authMethod.value === 'key_inline') {
    payload.private_key = form.value.private_key
    if (form.value.private_key_passphrase) payload.private_key_passphrase = form.value.private_key_passphrase
  } else if (authMethod.value === 'key_path') {
    payload.private_key_path = form.value.private_key_path
    if (form.value.private_key_passphrase) payload.private_key_passphrase = form.value.private_key_passphrase
  }

  emit('save', payload)
}
</script>
