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
