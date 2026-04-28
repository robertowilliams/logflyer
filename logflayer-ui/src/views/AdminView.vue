<template>
  <div class="space-y-6">

    <!-- Restart banner -->
    <div v-if="restartRequired"
      class="flex items-center gap-3 px-4 py-3 rounded-lg border border-[#f59e0b]/40 bg-[#f59e0b]/10 text-[#f59e0b] text-sm">
      <span class="text-lg">⚠️</span>
      <span>Settings saved — <strong>restart the logflayer container</strong> to apply changes.</span>
      <button @click="restartRequired = false" class="ml-auto opacity-60 hover:opacity-100">✕</button>
    </div>

    <!-- Error banner -->
    <div v-if="errorMsg"
      class="flex items-center gap-3 px-4 py-3 rounded-lg border border-[#dc143c]/40 bg-[#dc143c]/10 text-[#dc143c] text-sm">
      <span class="text-lg">✕</span>
      <span>{{ errorMsg }}</span>
      <button @click="errorMsg = ''" class="ml-auto opacity-60 hover:opacity-100">✕</button>
    </div>

    <!-- Loading state -->
    <div v-if="loading" class="text-center py-16 text-[rgba(245,245,220,0.40)]">
      Loading settings…
    </div>

    <template v-else>
      <!-- ── Sampling ─────────────────────────────────────────────────────── -->
      <section class="card">
        <h2 class="section-title">🔁 Sampling</h2>
        <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div class="field">
            <label>Sample mode</label>
            <select v-model="form.sample_mode" class="input-field">
              <option value="head">head — first N lines</option>
              <option value="tail">tail — last N lines</option>
              <option value="both">both — head + tail</option>
            </select>
          </div>
          <div class="field">
            <label>Lines per file <span class="hint">(SAMPLE_LINE_COUNT)</span></label>
            <input v-model.number="form.sample_line_count" type="number" min="1" class="input-field" />
          </div>
        </div>
      </section>

      <!-- ── Service ────────────────────────────────────────────────────────── -->
      <section class="card">
        <h2 class="section-title">⚙️ Service</h2>
        <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div class="field">
            <label>Run mode</label>
            <select v-model="form.run_mode" class="input-field">
              <option value="once">once — single pass then exit</option>
              <option value="periodic">periodic — poll on interval</option>
            </select>
          </div>
          <div class="field">
            <label>Poll interval <span class="hint">(seconds, periodic only)</span></label>
            <input v-model.number="form.poll_interval_secs" type="number" min="1" class="input-field" />
          </div>
          <div class="field">
            <label>Concurrency <span class="hint">(parallel targets)</span></label>
            <input v-model.number="form.concurrency" type="number" min="1" class="input-field" />
          </div>
          <div class="field">
            <label>SSH timeout <span class="hint">(seconds)</span></label>
            <input v-model.number="form.ssh_timeout_secs" type="number" min="1" class="input-field" />
          </div>
        </div>
      </section>

      <!-- ── Remote discovery ───────────────────────────────────────────────── -->
      <section class="card">
        <h2 class="section-title">🔍 Remote File Discovery</h2>
        <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div class="field">
            <label>Max directory depth <span class="hint">(REMOTE_MAX_DEPTH)</span></label>
            <input v-model.number="form.remote_max_depth" type="number" min="1" class="input-field" />
          </div>
          <div class="field">
            <label>Max files per target <span class="hint">(REMOTE_MAX_FILES_PER_TARGET)</span></label>
            <input v-model.number="form.remote_max_files_per_target" type="number" min="1" class="input-field" />
          </div>
          <div class="field md:col-span-2">
            <label>File patterns <span class="hint">(comma-separated, e.g. *.log,*.out)</span></label>
            <input v-model="form.remote_find_patterns" type="text" class="input-field"
              placeholder="*.log,*.out,*.txt" />
          </div>
        </div>
      </section>

      <!-- ── Preprocessing ─────────────────────────────────────────────────── -->
      <section class="card">
        <h2 class="section-title">🧹 Preprocessing Pipeline</h2>
        <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div class="field md:col-span-2">
            <toggle-field v-model="form.preprocessing_enabled" label="Enable preprocessing" />
          </div>
          <div class="field">
            <label>Agentic signal threshold <span class="hint">(0.0 – 1.0)</span></label>
            <input v-model.number="form.preprocessing_agentic_threshold"
              type="number" step="0.01" min="0.001" max="1" class="input-field" />
          </div>
          <div class="field">
            <label>Max schema lines</label>
            <input v-model.number="form.preprocessing_max_schema_lines"
              type="number" min="1" class="input-field" />
          </div>
        </div>
      </section>

      <!-- ── LLM Classification ─────────────────────────────────────────────── -->
      <section class="card">
        <h2 class="section-title">🧠 LLM Classification</h2>
        <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div class="field md:col-span-2">
            <toggle-field v-model="form.classification_enabled" label="Enable LLM classification" />
          </div>
          <div class="field">
            <label>API format</label>
            <select v-model="form.classification_api_format" class="input-field">
              <option value="anthropic">Anthropic (Claude)</option>
              <option value="openai">OpenAI-compatible (OpenAI, OpenRouter, Ollama, Groq, LM Studio…)</option>
            </select>
          </div>
          <div class="field">
            <label>API base URL
              <span class="hint">(leave empty for provider default)</span>
            </label>
            <input v-model="form.classification_api_base_url" type="text" class="input-field font-mono"
              :placeholder="apiBaseUrlPlaceholder" />
          </div>
          <div class="field md:col-span-2">
            <label>API key
              <span class="hint">
                {{ apiKeyIsSet ? '(currently set — enter a new value to replace, or leave as-is)' : '(not set)' }}
              </span>
            </label>
            <input v-model="form.anthropic_api_key" type="password"
              :placeholder="apiKeyIsSet ? '••••••••' : apiKeyPlaceholder"
              autocomplete="new-password" class="input-field font-mono" />
          </div>
          <!-- ── Model field: smart dropdown with free-text fallback ─────── -->
          <div class="field md:col-span-2">
            <div class="flex items-center justify-between mb-1">
              <label>
                Model
                <span v-if="modelsError" class="hint text-[#f59e0b]"> — {{ modelsError }}</span>
              </label>
              <div class="flex items-center gap-3 text-xs">
                <span v-if="modelsLoading" class="text-[rgba(245,245,220,0.40)] flex items-center gap-1">
                  <svg class="animate-spin w-3 h-3" viewBox="0 0 24 24" fill="none">
                    <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"/>
                    <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8v8z"/>
                  </svg>
                  Fetching models…
                </span>
                <button v-else-if="canFetchModels" @click="fetchModels"
                  class="text-[#00d4ff] hover:underline">
                  🔄 Fetch models
                </button>
                <button v-if="availableModels.length > 0 && !modelManualMode"
                  @click="modelManualMode = true"
                  class="text-[rgba(245,245,220,0.35)] hover:text-[rgba(245,245,220,0.70)]">
                  ✏️ type manually
                </button>
                <button v-if="modelManualMode && availableModels.length > 0"
                  @click="modelManualMode = false"
                  class="text-[rgba(245,245,220,0.35)] hover:text-[rgba(245,245,220,0.70)]">
                  📋 show list
                </button>
              </div>
            </div>

            <!-- Dropdown when models were fetched and user hasn't switched to manual -->
            <select
              v-if="availableModels.length > 0 && !modelManualMode && !modelsLoading"
              v-model="form.classification_model"
              class="input-field">
              <option value="">— select a model —</option>
              <option v-for="m in availableModels" :key="m" :value="m">{{ m }}</option>
            </select>

            <!-- Free-text input: loading state, failed fetch, empty list, or manual mode -->
            <input v-else
              v-model="form.classification_model"
              type="text"
              class="input-field font-mono"
              :class="{ 'opacity-50 cursor-not-allowed': modelsLoading }"
              :placeholder="modelPlaceholder"
              :disabled="modelsLoading" />
          </div>
          <div class="field">
            <label>Signal threshold <span class="hint">(min score to classify)</span></label>
            <input v-model.number="form.classification_signal_threshold"
              type="number" step="0.01" min="0" max="1" class="input-field" />
          </div>
          <div class="field">
            <label>Max API calls per cycle <span class="hint">(cost guard)</span></label>
            <input v-model.number="form.classification_max_per_cycle"
              type="number" min="1" class="input-field" />
          </div>
          <div class="field">
            <label>Max output tokens</label>
            <input v-model.number="form.classification_max_output_tokens"
              type="number" min="128" class="input-field" />
          </div>
        </div>
      </section>

      <!-- ── Notifications ─────────────────────────────────────────────────── -->
      <section class="card">
        <h2 class="section-title">🔔 Notifications</h2>
        <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div class="field md:col-span-2">
            <toggle-field v-model="form.notification_enabled" label="Enable notifications" />
          </div>
          <div class="field">
            <label>Minimum severity</label>
            <select v-model="form.notification_severity_threshold" class="input-field">
              <option value="critical">critical</option>
              <option value="warning">warning</option>
              <option value="info">info</option>
              <option value="normal">normal (all)</option>
            </select>
          </div>
          <div class="field">
            <label>Slack webhook URL</label>
            <input v-model="form.slack_webhook_url" type="text" class="input-field font-mono"
              placeholder="https://hooks.slack.com/services/..." />
          </div>
          <div class="field">
            <label>Generic webhook URL</label>
            <input v-model="form.webhook_url" type="text" class="input-field font-mono"
              placeholder="https://your-endpoint.example.com/hook" />
          </div>
          <div class="field">
            <label>Webhook signing secret
              <span class="hint">
                {{ webhookSecretIsSet ? '(currently set — enter a new value to replace)' : '(optional)' }}
              </span>
            </label>
            <input v-model="form.webhook_secret" type="password"
              :placeholder="webhookSecretIsSet ? '••••••••' : 'optional HMAC secret'"
              autocomplete="new-password" class="input-field font-mono" />
          </div>
        </div>
      </section>

      <!-- ── Logging ───────────────────────────────────────────────────────── -->
      <section class="card">
        <h2 class="section-title">📋 Logging</h2>
        <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div class="field">
            <label>Log level</label>
            <select v-model="form.log_level" class="input-field">
              <option value="error">error</option>
              <option value="warn">warn</option>
              <option value="info">info</option>
              <option value="debug">debug</option>
              <option value="trace">trace</option>
            </select>
          </div>
        </div>
      </section>

      <!-- ── Save bar ──────────────────────────────────────────────────────── -->
      <div class="flex items-center justify-between px-4 py-3 rounded-lg border border-[#dc143c]/20 bg-[#0f0f0f]">
        <span class="text-sm text-[rgba(245,245,220,0.50)]">
          Changes are applied on the next container restart.
        </span>
        <button @click="save" :disabled="saving"
          class="btn-primary flex items-center gap-2 disabled:opacity-50">
          <span v-if="saving">⏳ Saving…</span>
          <span v-else>💾 Save settings</span>
        </button>
      </div>
    </template>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from 'vue'
import { client } from '../api/client'
import type { AdminSettings } from '../types'

// ── State ─────────────────────────────────────────────────────────────────────

const loading         = ref(true)
const saving          = ref(false)
const restartRequired = ref(false)
const errorMsg        = ref('')

// Sensitive-field masking
const apiKeyIsSet        = ref(false)
const webhookSecretIsSet = ref(false)

const form = ref<AdminSettings>({})

// ── Provider-aware computed placeholders ──────────────────────────────────────

const isOpenAi = computed(() => form.value.classification_api_format === 'openai')

const apiKeyPlaceholder = computed(() =>
  isOpenAi.value ? 'sk-...' : 'sk-ant-...'
)
const modelPlaceholder = computed(() =>
  isOpenAi.value ? 'gpt-4o-mini' : 'claude-haiku-4-5-20251001'
)
const apiBaseUrlPlaceholder = computed(() =>
  isOpenAi.value
    ? 'https://api.openai.com  (or http://localhost:11434 for Ollama)'
    : 'https://api.anthropic.com'
)

// ── Model auto-fetch ──────────────────────────────────────────────────────────

const availableModels = ref<string[]>([])
const modelsLoading   = ref(false)
const modelsError     = ref('')
const modelManualMode = ref(false)

// We can fetch models when there is an API key available (either typed or stored)
const canFetchModels = computed(() =>
  !!(form.value.anthropic_api_key?.trim()) || apiKeyIsSet.value
)

let debounceTimer: ReturnType<typeof setTimeout> | null = null

async function fetchModels() {
  if (modelsLoading.value) return
  modelsLoading.value = true
  modelsError.value   = ''
  try {
    // Send "***" when the key is stored but the user hasn't typed a replacement;
    // the backend knows to use the config key in that case.
    const apiKey  = form.value.anthropic_api_key?.trim() || (apiKeyIsSet.value ? '***' : '')
    const baseUrl = form.value.classification_api_base_url?.trim() || ''
    const result  = await client.fetchModels(baseUrl, apiKey)

    if (result.ok && result.models.length > 0) {
      availableModels.value = result.models
      modelsError.value     = ''
      // If the current model value is in the list, keep it; otherwise leave as-is
    } else {
      availableModels.value = []
      modelsError.value     = result.error || 'Unable to fetch models automatically'
    }
  } catch {
    availableModels.value = []
    modelsError.value     = 'Unable to fetch models automatically'
  } finally {
    modelsLoading.value = false
  }
}

// Auto-fetch with debounce when API key or base URL changes
watch(
  [
    () => form.value.anthropic_api_key,
    () => form.value.classification_api_base_url,
    () => form.value.classification_api_format,
  ],
  () => {
    if (debounceTimer) clearTimeout(debounceTimer)
    // Reset model list when provider changes so stale options don't linger
    availableModels.value = []
    modelsError.value     = ''
    if (canFetchModels.value) {
      debounceTimer = setTimeout(fetchModels, 800)
    }
  }
)

onUnmounted(() => { if (debounceTimer) clearTimeout(debounceTimer) })

// ── Load ──────────────────────────────────────────────────────────────────────

onMounted(async () => {
  try {
    const { settings } = await client.getAdminSettings()
    apiKeyIsSet.value        = settings.anthropic_api_key === '***'
    webhookSecretIsSet.value = settings.webhook_secret   === '***'
    if (apiKeyIsSet.value)        settings.anthropic_api_key = ''
    if (webhookSecretIsSet.value) settings.webhook_secret   = ''
    form.value = { ...settings }
    // Kick off model fetch if we already have a stored key
    if (apiKeyIsSet.value) fetchModels()
  } catch {
    errorMsg.value = 'Failed to load settings from the API.'
  } finally {
    loading.value = false
  }
})

// ── Save ──────────────────────────────────────────────────────────────────────

async function save() {
  saving.value  = false
  errorMsg.value = ''
  saving.value  = true
  try {
    // Build the payload, restoring "***" sentinels for unchanged sensitive fields.
    const payload: AdminSettings = { ...form.value }
    if (apiKeyIsSet.value && !payload.anthropic_api_key)  payload.anthropic_api_key = '***'
    if (webhookSecretIsSet.value && !payload.webhook_secret) payload.webhook_secret = '***'
    // If the user typed a real value, update the "is set" tracking.
    if (payload.anthropic_api_key && payload.anthropic_api_key !== '***') apiKeyIsSet.value = true
    if (payload.webhook_secret   && payload.webhook_secret   !== '***') webhookSecretIsSet.value = true

    await client.saveAdminSettings(payload)
    restartRequired.value = true
  } catch (e) {
    errorMsg.value = 'Failed to save settings. Check that the API is reachable.'
  } finally {
    saving.value = false
  }
}
</script>

<!-- ToggleField helper component (inline) -->
<script lang="ts">
import { defineComponent, h } from 'vue'

export const ToggleField = defineComponent({
  name: 'ToggleField',
  props: { modelValue: Boolean, label: String },
  emits: ['update:modelValue'],
  setup(props, { emit }) {
    return () => h('label', { class: 'flex items-center gap-3 cursor-pointer select-none' }, [
      h('div', {
        class: [
          'relative w-11 h-6 rounded-full transition-colors duration-200',
          props.modelValue ? 'bg-[#dc143c]' : 'bg-[rgba(245,245,220,0.15)]',
        ].join(' '),
        onClick: () => emit('update:modelValue', !props.modelValue),
      }, [
        h('div', {
          class: [
            'absolute top-0.5 left-0.5 w-5 h-5 rounded-full bg-white transition-transform duration-200',
            props.modelValue ? 'translate-x-5' : 'translate-x-0',
          ].join(' '),
        }),
      ]),
      h('span', { class: 'text-sm text-[rgba(245,245,220,0.80)]' }, props.label),
    ])
  },
})

export default { components: { ToggleField } }
</script>

<style scoped>
.card {
  background: #0f0f0f;
  border: 1px solid rgba(220, 20, 60, 0.2);
  border-radius: 0.5rem;
  padding: 1.25rem 1.5rem;
}

.section-title {
  font-size: 0.9rem;
  font-weight: 600;
  color: #f5f5dc;
  margin-bottom: 1rem;
  letter-spacing: 0.03em;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 0.375rem;
}

.field label {
  font-size: 0.78rem;
  font-weight: 500;
  color: rgba(245, 245, 220, 0.65);
}

.hint {
  font-weight: 400;
  opacity: 0.6;
  font-size: 0.72rem;
}

.input-field {
  background: #1a1a1a;
  border: 1px solid rgba(220, 20, 60, 0.25);
  border-radius: 0.375rem;
  padding: 0.45rem 0.75rem;
  color: #f5f5dc;
  font-size: 0.85rem;
  transition: border-color 0.15s;
  width: 100%;
}

.input-field:focus {
  outline: none;
  border-color: rgba(220, 20, 60, 0.6);
  box-shadow: 0 0 0 2px rgba(220, 20, 60, 0.12);
}

.input-field option {
  background: #1a1a1a;
}

.btn-primary {
  background: linear-gradient(135deg, #dc143c, #a00028);
  color: #f5f5dc;
  border: none;
  border-radius: 0.375rem;
  padding: 0.5rem 1.25rem;
  font-size: 0.85rem;
  font-weight: 600;
  cursor: pointer;
  transition: opacity 0.15s;
}

.btn-primary:hover:not(:disabled) {
  opacity: 0.88;
}
</style>
