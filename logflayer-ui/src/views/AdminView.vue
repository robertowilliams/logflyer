<template>
  <div class="space-y-6">

    <!-- Restart banner -->
    <div v-if="restartRequired && !restarting"
      class="flex items-center gap-3 px-4 py-3 rounded-lg border border-[#f59e0b]/40 bg-[#f59e0b]/10 text-[#f59e0b] text-sm">
      <AlertTriangle :size="18" class="shrink-0" />
      <span>Settings saved — restart the container to apply changes.</span>
      <button @click="triggerRestart"
        class="ml-auto px-3 py-1 rounded border border-[#f59e0b]/50 hover:border-[#f59e0b]
               text-xs font-medium transition-colors whitespace-nowrap inline-flex items-center gap-1.5">
        <RotateCw :size="13" />Restart now
      </button>
      <button @click="restartRequired = false" class="opacity-60 hover:opacity-100"><X :size="16" /></button>
    </div>

    <!-- Error banner -->
    <div v-if="errorMsg"
      class="flex items-center gap-3 px-4 py-3 rounded-lg border border-[#dc143c]/40 bg-[#dc143c]/10 text-[#dc143c] text-sm">
      <AlertCircle :size="18" class="shrink-0" />
      <span>{{ errorMsg }}</span>
      <button @click="errorMsg = ''" class="ml-auto opacity-60 hover:opacity-100"><X :size="16" /></button>
    </div>

    <!-- Loading state -->
    <div v-if="loading" class="text-center py-16 text-[rgba(245,245,220,0.40)]">
      Loading settings…
    </div>

    <template v-else>
      <!-- ── MongoDB ───────────────────────────────────────────────────────── -->
      <section class="card">
        <h2 class="section-title flex items-center gap-1.5"><Database :size="14" />MongoDB</h2>
        <p class="text-xs text-[rgba(245,245,220,0.40)] mb-4 flex items-start gap-1.5">
          <AlertTriangle :size="13" class="shrink-0 mt-0.5" />
          <span>Changes here take effect on the next container restart. If the URI is wrong the service will fall back to the env-var value on startup.</span>
        </p>
        <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div class="field md:col-span-2">
            <label>MongoDB URI
              <span class="hint">
                {{ mongoUriIsSet ? '(currently set — enter new value to replace)' : '(MONGODB_URI)' }}
              </span>
            </label>
            <input v-model="form.mongodb_uri" type="password"
              :placeholder="mongoUriIsSet ? '••••••••' : 'mongodb://localhost:27017'"
              autocomplete="new-password" class="input-field font-mono" />
          </div>
          <div class="field">
            <label>Source DB <span class="hint">(SOURCE_DB_NAME)</span></label>
            <input v-model="form.source_db_name" type="text" class="input-field font-mono"
              placeholder="vectadb" />
          </div>
          <div class="field">
            <label>Source collection <span class="hint">(SOURCE_COLLECTION_NAME)</span></label>
            <input v-model="form.source_collection_name" type="text" class="input-field font-mono"
              placeholder="ai_targets" />
          </div>
          <div class="field">
            <label>Destination DB <span class="hint">(DESTINATION_DB_NAME)</span></label>
            <input v-model="form.destination_db_name" type="text" class="input-field font-mono"
              placeholder="log_samples" />
          </div>
          <div class="field">
            <label>Tracking DB <span class="hint">(TRACKING_DB_NAME)</span></label>
            <input v-model="form.tracking_db_name" type="text" class="input-field font-mono"
              placeholder="loggingtracker" />
          </div>
          <div class="field">
            <label>Tracking collection <span class="hint">(TRACKING_COLLECTION_NAME)</span></label>
            <input v-model="form.tracking_collection_name" type="text" class="input-field font-mono"
              placeholder="logging_tracks" />
          </div>
        </div>
      </section>

      <!-- ── Sampling ─────────────────────────────────────────────────────── -->
      <section class="card">
        <h2 class="section-title flex items-center gap-1.5"><Repeat :size="14" />Sampling</h2>
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
        <h2 class="section-title flex items-center gap-1.5"><Settings :size="14" />Service</h2>
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
          <div class="field">
            <label>API port <span class="hint">(API_PORT — requires restart)</span></label>
            <input v-model.number="form.api_port" type="number" min="1" max="65535" class="input-field" />
          </div>
        </div>
      </section>

      <!-- ── Remote discovery ───────────────────────────────────────────────── -->
      <section class="card">
        <h2 class="section-title flex items-center gap-1.5"><Search :size="14" />Remote File Discovery</h2>
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
        <h2 class="section-title flex items-center gap-1.5"><SlidersHorizontal :size="14" />Preprocessing Pipeline</h2>
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
          <div class="field">
            <label>Prometheus metrics port <span class="hint">(METRICS_PORT — requires restart)</span></label>
            <input v-model.number="form.metrics_port" type="number" min="1" max="65535" class="input-field" />
          </div>
        </div>
      </section>

      <!-- ── LLM Classification ─────────────────────────────────────────────── -->
      <section class="card">
        <h2 class="section-title flex items-center gap-1.5"><Brain :size="14" />LLM Classification</h2>
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
                  class="text-[#00d4ff] hover:underline inline-flex items-center gap-1.5">
                  <RotateCw :size="13" />Fetch models
                </button>
                <button v-if="availableModels.length > 0 && !modelManualMode"
                  @click="modelManualMode = true"
                  class="text-[rgba(245,245,220,0.35)] hover:text-[rgba(245,245,220,0.70)] inline-flex items-center gap-1.5">
                  <Pencil :size="13" />type manually
                </button>
                <button v-if="modelManualMode && availableModels.length > 0"
                  @click="modelManualMode = false"
                  class="text-[rgba(245,245,220,0.35)] hover:text-[rgba(245,245,220,0.70)] inline-flex items-center gap-1.5">
                  <List :size="13" />show list
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
        <h2 class="section-title flex items-center gap-1.5"><Bell :size="14" />Notifications</h2>
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
        <h2 class="section-title flex items-center gap-1.5"><ScrollText :size="14" />Logging</h2>
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
          <div class="field">
            <label>Log directory <span class="hint">(LOG_DIRECTORY)</span></label>
            <input v-model="form.log_directory" type="text" class="input-field font-mono"
              placeholder="./logs" />
          </div>
          <div class="field">
            <label>Log file base name <span class="hint">(LOG_FILE_BASE_NAME)</span></label>
            <input v-model="form.log_file_base_name" type="text" class="input-field font-mono"
              placeholder="logflayer" />
          </div>
          <div class="field">
            <label>Max file size <span class="hint">(bytes — LOG_MAX_FILE_SIZE_BYTES)</span></label>
            <input v-model.number="form.log_max_file_size_bytes" type="number" min="1" class="input-field" />
          </div>
          <div class="field">
            <label>Max log files retained <span class="hint">(LOG_MAX_FILES)</span></label>
            <input v-model.number="form.log_max_files" type="number" min="1" class="input-field" />
          </div>
        </div>
      </section>

      <!-- ── Config history ────────────────────────────────────────────────── -->
      <section class="card">
        <h2 class="section-title flex items-center gap-1.5"><Lock :size="14" />Configuration History</h2>
        <p class="text-xs text-[rgba(245,245,220,0.40)] mb-4">
          When enabled, every settings save creates an encrypted snapshot in MongoDB.
          Secret fields are AES-256-GCM encrypted using the master key — store the key safely outside the database.
          Generate a key: <code class="font-mono">openssl rand -base64 32</code>
        </p>
        <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div class="field md:col-span-2">
            <toggle-field v-model="form.config_history_enabled" label="Enable configuration history" />
          </div>
          <div class="field md:col-span-2">
            <label>Master key
              <span class="hint">
                {{ historyMasterKeyIsSet ? '(currently set — enter new value to rotate)' : '(CONFIG_HISTORY_MASTER_KEY — base64 32-byte key)' }}
              </span>
            </label>
            <input v-model="form.config_history_master_key" type="password"
              :placeholder="historyMasterKeyIsSet ? '••••••••' : 'base64-encoded 32-byte key'"
              autocomplete="new-password" class="input-field font-mono" />
          </div>
          <div class="field">
            <label>Key ID <span class="hint">(CONFIG_HISTORY_KEY_ID)</span></label>
            <input v-model="form.config_history_key_id" type="text" class="input-field font-mono"
              placeholder="logflayer-config-v1" />
          </div>
          <div class="field">
            <label>Collection name <span class="hint">(CONFIG_HISTORY_COLLECTION_NAME)</span></label>
            <input v-model="form.config_history_collection_name" type="text" class="input-field font-mono"
              placeholder="app_settings_history" />
          </div>
        </div>
      </section>

      <!-- ── Save / Restart bar ───────────────────────────────────────────── -->
      <div class="flex items-center justify-between gap-3 px-4 py-3 rounded-lg border border-[#dc143c]/20 bg-[#0f0f0f]">
        <span class="text-sm text-[rgba(245,245,220,0.50)]">
          Changes are applied on restart.
        </span>
        <div class="flex items-center gap-2">
          <button @click="save" :disabled="saving || restarting"
            class="btn-primary flex items-center gap-2 disabled:opacity-50">
            <span v-if="saving" class="inline-flex items-center gap-1.5"><Loader2 :size="14" class="animate-spin" />Saving…</span>
            <span v-else class="inline-flex items-center gap-1.5"><Save :size="14" />Save settings</span>
          </button>
          <button @click="triggerRestart" :disabled="saving || restarting"
            class="flex items-center gap-2 px-4 py-2 rounded text-sm font-medium transition-colors
                   border border-[rgba(245,245,220,0.20)] text-[rgba(245,245,220,0.70)]
                   hover:border-[rgba(245,245,220,0.40)] hover:text-[rgba(245,245,220,0.90)]
                   disabled:opacity-40">
            <span v-if="restarting" class="inline-flex items-center gap-1.5"><Loader2 :size="14" class="animate-spin" />Restarting…</span>
            <span v-else class="inline-flex items-center gap-1.5"><RotateCw :size="14" />Save &amp; Restart</span>
          </button>
        </div>
      </div>

      <!-- Restarting overlay banner -->
      <div v-if="restarting"
        class="flex items-center gap-3 px-4 py-3 rounded-lg border border-[rgba(245,245,220,0.15)] bg-[#0f0f0f] text-sm text-[rgba(245,245,220,0.65)]">
        <svg class="animate-spin w-4 h-4 shrink-0" viewBox="0 0 24 24" fill="none">
          <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"/>
          <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8v8H4z"/>
        </svg>
        <span>Container is restarting — waiting for it to come back online…</span>
        <span class="ml-auto font-mono text-xs opacity-50">{{ restartElapsed }}s</span>
      </div>

      <!-- ── Configuration history ─────────────────────────────────────────── -->
      <section v-if="historyEntries.length > 0 || historyLoading" class="card">
        <div class="flex items-center justify-between mb-4">
          <h2 class="section-title mb-0 flex items-center gap-1.5"><History :size="14" />Configuration History</h2>
          <button @click="loadHistory" :disabled="historyLoading"
            class="text-xs px-2 py-1 rounded border border-[rgba(245,245,220,0.15)]
                   text-[rgba(245,245,220,0.50)] hover:text-[rgba(245,245,220,0.80)]
                   hover:border-[rgba(245,245,220,0.30)] disabled:opacity-40 transition-colors">
            <span v-if="historyLoading" class="inline-flex items-center gap-1.5"><Loader2 :size="12" class="animate-spin" />Loading…</span>
            <span v-else class="inline-flex items-center gap-1.5"><RefreshCw :size="12" />Refresh</span>
          </button>
        </div>

        <div v-if="historyLoading" class="text-center py-6 text-[rgba(245,245,220,0.40)] text-sm">
          Loading history…
        </div>

        <div v-else-if="historyEntries.length === 0"
          class="text-center py-6 text-[rgba(245,245,220,0.35)] text-sm">
          No history entries yet. Save settings to create the first snapshot.
        </div>

        <div v-else class="overflow-x-auto">
          <table class="w-full text-sm">
            <thead>
              <tr class="border-b border-[rgba(245,245,220,0.08)]">
                <th class="text-left py-2 pr-4 text-[rgba(245,245,220,0.45)] font-medium text-xs">Version</th>
                <th class="text-left py-2 pr-4 text-[rgba(245,245,220,0.45)] font-medium text-xs">Saved at</th>
                <th class="text-left py-2 pr-4 text-[rgba(245,245,220,0.45)] font-medium text-xs">Reason</th>
                <th class="text-left py-2    text-[rgba(245,245,220,0.45)] font-medium text-xs">Action</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="entry in historyEntries" :key="entry.version"
                class="border-b border-[rgba(245,245,220,0.04)] hover:bg-[rgba(245,245,220,0.02)]">
                <td class="py-2 pr-4 font-mono text-[rgba(245,245,220,0.70)]">v{{ entry.version }}</td>
                <td class="py-2 pr-4 text-[rgba(245,245,220,0.55)] whitespace-nowrap">
                  {{ formatTs(entry.created_at) }}
                </td>
                <td class="py-2 pr-4 text-[rgba(245,245,220,0.65)]">{{ entry.reason }}</td>
                <td class="py-2">
                  <button
                    @click="restore(entry.version)"
                    :disabled="restoringVersion === entry.version"
                    class="text-xs px-2 py-1 rounded border border-[#dc143c]/30
                           text-[#dc143c]/70 hover:text-[#dc143c] hover:border-[#dc143c]/60
                           disabled:opacity-40 transition-colors">
                    {{ restoringVersion === entry.version ? '⏳ Restoring…' : '⏎ Restore' }}
                  </button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>

        <!-- Restore result banner -->
        <div v-if="restoreMsg"
          class="mt-3 flex items-center gap-2 px-3 py-2 rounded text-xs"
          :class="restoreMsgOk
            ? 'border border-green-700/40 bg-green-900/10 text-green-400'
            : 'border border-[#dc143c]/40 bg-[#dc143c]/10 text-[#dc143c]'">
          {{ restoreMsg }}
          <button @click="restoreMsg = ''" class="ml-auto opacity-60 hover:opacity-100"><X :size="16" /></button>
        </div>
      </section>

    </template>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from 'vue'
import {
  AlertTriangle, AlertCircle, RotateCw, RefreshCw, X, Database, Repeat, Settings,
  Search, SlidersHorizontal, Brain, Bell, ScrollText, Lock, History, Pencil, List, Save, Loader2,
} from 'lucide-vue-next'
import { client } from '../api/client'
import type { AdminSettings, HistoryEntry } from '../types'

// ── State ─────────────────────────────────────────────────────────────────────

const loading         = ref(true)
const saving          = ref(false)
const restarting      = ref(false)
const restartElapsed  = ref(0)
const restartRequired = ref(false)
const errorMsg        = ref('')

// Sensitive-field masking ("***" sentinel tracking)
const apiKeyIsSet             = ref(false)
const webhookSecretIsSet      = ref(false)
const mongoUriIsSet           = ref(false)
const historyMasterKeyIsSet   = ref(false)

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
    apiKeyIsSet.value           = settings.anthropic_api_key      === '***'
    webhookSecretIsSet.value    = settings.webhook_secret         === '***'
    mongoUriIsSet.value         = settings.mongodb_uri            === '***'
    historyMasterKeyIsSet.value = settings.config_history_master_key === '***'
    if (apiKeyIsSet.value)           settings.anthropic_api_key         = ''
    if (webhookSecretIsSet.value)    settings.webhook_secret            = ''
    if (mongoUriIsSet.value)         settings.mongodb_uri               = ''
    if (historyMasterKeyIsSet.value) settings.config_history_master_key = ''
    form.value = { ...settings }
    // Kick off model fetch if we already have a stored key
    if (apiKeyIsSet.value) fetchModels()
    // Load history in the background — non-fatal if disabled
    loadHistory()
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
    // Restore *** sentinels for fields the user left blank (meaning "don't change").
    if (apiKeyIsSet.value           && !payload.anthropic_api_key)         payload.anthropic_api_key         = '***'
    if (webhookSecretIsSet.value    && !payload.webhook_secret)            payload.webhook_secret            = '***'
    if (mongoUriIsSet.value         && !payload.mongodb_uri)               payload.mongodb_uri               = '***'
    if (historyMasterKeyIsSet.value && !payload.config_history_master_key) payload.config_history_master_key = '***'
    // Update "is set" tracking when the user provides a real value.
    if (payload.anthropic_api_key         && payload.anthropic_api_key         !== '***') apiKeyIsSet.value           = true
    if (payload.webhook_secret            && payload.webhook_secret            !== '***') webhookSecretIsSet.value    = true
    if (payload.mongodb_uri               && payload.mongodb_uri               !== '***') mongoUriIsSet.value         = true
    if (payload.config_history_master_key && payload.config_history_master_key !== '***') historyMasterKeyIsSet.value = true

    await client.saveAdminSettings(payload)
    await client.confirmSettings()
    restartRequired.value = true
    // Refresh history so the new snapshot appears immediately.
    loadHistory()
  } catch (e) {
    errorMsg.value = 'Failed to save settings. Check that the API is reachable.'
  } finally {
    saving.value = false
  }
}

// ── Restart ───────────────────────────────────────────────────────────────────

async function triggerRestart() {
  if (!confirm('Restart the logflayer container now? It will be unavailable for a few seconds while it comes back up.')) return

  restarting.value     = true
  restartElapsed.value = 0
  restartRequired.value = false
  errorMsg.value       = ''

  // Elapsed-seconds counter while we wait
  const ticker = setInterval(() => { restartElapsed.value++ }, 1000)

  try {
    // Fire the restart request — the process will exit ~500 ms after this returns.
    await client.restartService()
  } catch {
    // The request may fail or be cut off as the server exits — that's expected.
  }

  // Poll /health until the service responds again (max 60 s).
  const deadline = Date.now() + 60_000
  let back = false
  while (Date.now() < deadline) {
    await new Promise(r => setTimeout(r, 2000))
    try {
      await client.health()
      back = true
      break
    } catch {
      // Still down — keep waiting
    }
  }

  clearInterval(ticker)
  restarting.value = false

  if (back) {
    // Reload the settings form with the fresh config that was just applied.
    try {
      const { settings } = await client.getAdminSettings()
      apiKeyIsSet.value           = settings.anthropic_api_key         === '***'
      webhookSecretIsSet.value    = settings.webhook_secret            === '***'
      mongoUriIsSet.value         = settings.mongodb_uri               === '***'
      historyMasterKeyIsSet.value = settings.config_history_master_key === '***'
      if (apiKeyIsSet.value)           settings.anthropic_api_key         = ''
      if (webhookSecretIsSet.value)    settings.webhook_secret            = ''
      if (mongoUriIsSet.value)         settings.mongodb_uri               = ''
      if (historyMasterKeyIsSet.value) settings.config_history_master_key = ''
      form.value = { ...settings }
      loadHistory()
    } catch {
      // Non-fatal — form stays as-is
    }
  } else {
    errorMsg.value = 'Restart timed out — the container may still be starting. Refresh the page to check.'
  }
}

// ── Configuration history ─────────────────────────────────────────────────────

const historyEntries  = ref<HistoryEntry[]>([])
const historyLoading  = ref(false)
const restoringVersion = ref<number | null>(null)
const restoreMsg      = ref('')
const restoreMsgOk    = ref(false)

async function loadHistory() {
  historyLoading.value = true
  try {
    const { entries } = await client.getSettingsHistory()
    historyEntries.value = entries
  } catch {
    // History may be disabled — silently swallow the error.
  } finally {
    historyLoading.value = false
  }
}

async function restore(version: number) {
  if (!confirm(`Restore configuration to version ${version}? This will overwrite the current saved settings.`)) return
  restoringVersion.value = version
  restoreMsg.value = ''
  try {
    await client.restoreSettingsVersion(version)
    restoreMsgOk.value = true
    restoreMsg.value   = `Restored to v${version}. Reload the page to see the restored values.`
    restartRequired.value = true
    loadHistory()
  } catch {
    restoreMsgOk.value = false
    restoreMsg.value   = `Failed to restore version ${version}. Check that CONFIG_HISTORY_MASTER_KEY is set and matches the key used when the snapshot was recorded.`
  } finally {
    restoringVersion.value = null
  }
}

function formatTs(iso: string): string {
  if (!iso) return ''
  try {
    return new Date(iso).toLocaleString(undefined, {
      year: 'numeric', month: 'short', day: 'numeric',
      hour: '2-digit', minute: '2-digit',
    })
  } catch {
    return iso
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
