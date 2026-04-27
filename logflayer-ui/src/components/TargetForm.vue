<template>
  <form @submit.prevent="submit" class="space-y-5">
    <!-- Target ID -->
    <div>
      <label class="block text-xs font-medium text-[rgba(245,245,220,0.60)] mb-1">Target ID *</label>
      <input v-model="form.target_id" type="text" required class="input" placeholder="e.g. prod-web-01" />
    </div>

    <!-- Host / Port / User -->
    <div class="grid grid-cols-3 gap-3">
      <div class="col-span-2">
        <label class="block text-xs font-medium text-[rgba(245,245,220,0.60)] mb-1">Host *</label>
        <input v-model="form.host" type="text" required class="input" placeholder="192.168.1.10" />
      </div>
      <div>
        <label class="block text-xs font-medium text-[rgba(245,245,220,0.60)] mb-1">Port</label>
        <input v-model.number="form.port" type="number" class="input" placeholder="22" />
      </div>
    </div>

    <div>
      <label class="block text-xs font-medium text-[rgba(245,245,220,0.60)] mb-1">Username *</label>
      <input v-model="form.username" type="text" required class="input" placeholder="ubuntu" />
    </div>

    <!-- Auth method selector -->
    <div>
      <label class="block text-xs font-medium text-[rgba(245,245,220,0.60)] mb-2">Auth Method</label>
      <div class="flex gap-2 flex-wrap">
        <button
          v-for="m in authMethods" :key="m.value" type="button"
          @click="authMethod = m.value"
          :class="authMethod === m.value
            ? 'bg-[#dc143c] text-[#f5f5dc] shadow-[0_0_10px_rgba(220,20,60,0.4)]'
            : 'bg-[#1a1a1a] text-[rgba(245,245,220,0.60)] border border-[#dc143c]/20 hover:border-[#dc143c]/50'"
          class="px-3 py-1.5 rounded-lg text-xs font-medium transition-all duration-200"
        >
          {{ m.label }}
        </button>
      </div>
    </div>

    <!-- Password -->
    <div v-if="authMethod === 'password'">
      <label class="block text-xs font-medium text-[rgba(245,245,220,0.60)] mb-1">Password</label>
      <input v-model="form.password" type="password" class="input" placeholder="SSH password" />
    </div>

    <!-- Inline private key -->
    <div v-if="authMethod === 'key_inline'">
      <label class="block text-xs font-medium text-[rgba(245,245,220,0.60)] mb-1">Private Key (PEM)</label>
      <textarea
        v-model="form.private_key"
        rows="6"
        class="input font-mono text-xs resize-y"
        placeholder="-----BEGIN OPENSSH PRIVATE KEY-----&#10;...&#10;-----END OPENSSH PRIVATE KEY-----"
      />
      <p class="text-[rgba(245,245,220,0.30)] text-xs mt-1">Paste the full contents of your id_rsa or id_ed25519 file.</p>
    </div>

    <!-- Key file path -->
    <div v-if="authMethod === 'key_path'">
      <label class="block text-xs font-medium text-[rgba(245,245,220,0.60)] mb-1">Key File Path</label>
      <input v-model="form.private_key_path" type="text" class="input font-mono" placeholder="/root/.ssh/id_rsa" />
      <p class="text-[rgba(245,245,220,0.30)] text-xs mt-1">Path on the logflayer host machine.</p>
    </div>

    <!-- Log paths -->
    <div>
      <label class="block text-xs font-medium text-[rgba(245,245,220,0.60)] mb-1">Log Paths</label>
      <textarea
        v-model="logPathsText"
        rows="4"
        class="input font-mono text-xs resize-y"
        placeholder="/var/log/syslog&#10;/var/log/auth.log&#10;/var/log/nginx/*.log"
      />
      <p class="text-[rgba(245,245,220,0.30)] text-xs mt-1">One path per line. Glob patterns supported.</p>
    </div>

    <!-- Status -->
    <div>
      <label class="block text-xs font-medium text-[rgba(245,245,220,0.60)] mb-2">Status</label>
      <div class="flex gap-3">
        <label class="flex items-center gap-2 cursor-pointer">
          <input type="radio" v-model="form.status" value="active" class="accent-[#dc143c]" />
          <span class="text-sm text-[rgba(245,245,220,0.80)]">Active</span>
        </label>
        <label class="flex items-center gap-2 cursor-pointer">
          <input type="radio" v-model="form.status" value="inactive" class="accent-[#dc143c]" />
          <span class="text-sm text-[rgba(245,245,220,0.80)]">Inactive</span>
        </label>
      </div>
    </div>

    <!-- Actions -->
    <div class="flex gap-3 pt-2 border-t border-[#dc143c]/20">
      <button type="submit" class="btn-primary flex-1">
        {{ initial ? 'Save Changes' : 'Create Target' }}
      </button>
      <button type="button" @click="$emit('cancel')" class="btn-secondary">Cancel</button>
    </div>
  </form>
</template>

<script setup lang="ts">
import { ref, reactive, computed, watch } from 'vue'
import type { Target } from '../types'

const props = defineProps<{ initial?: Target | null }>()
const emit = defineEmits<{
  (e: 'save', data: Partial<Target>): void
  (e: 'cancel'): void
}>()

const authMethods = [
  { value: 'agent',      label: 'SSH Agent' },
  { value: 'password',   label: 'Password'  },
  { value: 'key_inline', label: 'Key Inline'},
  { value: 'key_path',   label: 'Key Path'  },
]

function detectAuthMethod(t?: Target | null) {
  if (!t) return 'agent'
  if (t.private_key || t.credentials?.private_key) return 'key_inline'
  if (t.private_key_path || t.credentials?.private_key_path) return 'key_path'
  if (t.password || t.credentials?.password) return 'password'
  return 'agent'
}

const authMethod = ref(detectAuthMethod(props.initial))

const form = reactive<Partial<Target> & { host?: string; username?: string; password?: string; private_key?: string; private_key_path?: string }>({
  target_id:        props.initial?.target_id        ?? '',
  host:             (props.initial as any)?.host || (props.initial as any)?.hostname || '',
  port:             (props.initial as any)?.port     ?? 22,
  username:         (props.initial as any)?.username || (props.initial as any)?.user || '',
  password:         (props.initial as any)?.password || props.initial?.credentials?.password || '',
  private_key:      (props.initial as any)?.private_key || props.initial?.credentials?.private_key || '',
  private_key_path: (props.initial as any)?.private_key_path || props.initial?.credentials?.private_key_path || '',
  status:           props.initial?.status ?? 'active',
})

const logPathsText = ref(
  ((props.initial as any)?.log_paths || (props.initial as any)?.log_dirs || []).join('\n')
)

watch(() => props.initial, (t) => {
  authMethod.value = detectAuthMethod(t)
  form.target_id        = t?.target_id        ?? ''
  form.host             = (t as any)?.host || (t as any)?.hostname || ''
  form.port             = (t as any)?.port     ?? 22
  form.username         = (t as any)?.username || (t as any)?.user || ''
  form.password         = (t as any)?.password || t?.credentials?.password || ''
  form.private_key      = (t as any)?.private_key || t?.credentials?.private_key || ''
  form.private_key_path = (t as any)?.private_key_path || t?.credentials?.private_key_path || ''
  form.status           = t?.status ?? 'active'
  logPathsText.value    = ((t as any)?.log_paths || (t as any)?.log_dirs || []).join('\n')
})

function submit() {
  const log_paths = logPathsText.value.split('\n').map(s => s.trim()).filter(Boolean)
  const payload: Record<string, unknown> = {
    target_id: form.target_id,
    host:      form.host,
    port:      form.port ?? 22,
    username:  form.username,
    status:    form.status,
    log_paths,
  }
  if (authMethod.value === 'password')   payload.password         = form.password
  if (authMethod.value === 'key_inline') payload.private_key      = form.private_key
  if (authMethod.value === 'key_path')   payload.private_key_path = form.private_key_path

  emit('save', payload as Partial<Target>)
}
</script>
