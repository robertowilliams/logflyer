export interface Target {
  id: string
  target_id: string
  status: 'active' | 'inactive' | string
  host?: string
  hostname?: string
  server?: string
  port?: number
  username?: string
  user?: string
  password?: string
  private_key?: string
  private_key_path?: string
  private_key_passphrase?: string
  log_paths?: string[]
  log_dirs?: string[]
  connection?: Record<string, unknown>
  credentials?: Record<string, unknown>
  /** How many lines to sample per file. Overrides global SAMPLE_LINE_COUNT. */
  sample_line_count?: number
  /** Max files to discover per log directory. Overrides global REMOTE_MAX_FILES_PER_TARGET. */
  max_files?: number
  [key: string]: unknown
}

export interface SampleRecord {
  id?: string
  timestamp: string
  target_id: string
  source_file: string
  sample_content: string
  host: string
  path: string
  sampling_mode: string
  line_count?: number
  file_size_bytes?: number
  processing_status: string
  error_details?: string
  sample_hash: string
}

export interface LogLine {
  raw: string
  level?: string
  timestamp?: string
  message?: string
}

export interface TrackingRecord {
  id?: string
  timestamp?: string
  level?: string
  message?: string
  [key: string]: unknown
}

export interface HealthResponse {
  status: 'healthy' | 'degraded'
  mongodb: string
  version: string
}

export interface PagedResponse<T> {
  records: T[]
  total: number
  page: number
  limit: number
}

export interface TargetsResponse {
  targets: Target[]
  total: number
}

export interface AdminSettings {
  // Sampling
  sample_mode?: string
  sample_line_count?: number
  // Service
  run_mode?: string
  poll_interval_secs?: number
  concurrency?: number
  ssh_timeout_secs?: number
  // Discovery
  remote_max_depth?: number
  remote_max_files_per_target?: number
  remote_find_patterns?: string
  // Preprocessing
  preprocessing_enabled?: boolean
  preprocessing_agentic_threshold?: number
  preprocessing_max_schema_lines?: number
  // Classification
  classification_enabled?: boolean
  anthropic_api_key?: string
  classification_model?: string
  classification_signal_threshold?: number
  classification_max_per_cycle?: number
  classification_max_output_tokens?: number
  classification_api_base_url?: string
  classification_api_format?: string
  // Notifications
  notification_enabled?: boolean
  notification_severity_threshold?: string
  slack_webhook_url?: string
  webhook_url?: string
  webhook_secret?: string
  // Logging
  log_level?: string
}

export interface SettingsResponse {
  settings: AdminSettings
  has_overrides: boolean
}

export interface Finding {
  pattern:  string
  count:    number
  severity: string
  example:  string
}

export interface ClassificationRecord {
  id?:                    string
  sample_hash:            string
  target_id:              string
  classified_at:          string
  model:                  string
  severity:               'critical' | 'warning' | 'info' | 'normal'
  categories:             string[]
  summary:                string
  key_findings:           Finding[]
  recommendations:        string[]
  confidence:             number
  input_tokens:           number
  output_tokens:          number
  classification_version: string
}
