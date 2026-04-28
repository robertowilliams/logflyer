import axios, { type AxiosInstance } from 'axios'
import type {
  Target, SampleRecord, TrackingRecord, HealthResponse,
  PagedResponse, TargetsResponse, ClassificationRecord,
  AdminSettings, SettingsResponse,
} from '../types'

class LogflayerClient {
  private http: AxiosInstance

  constructor(baseURL = 'http://localhost:8080') {
    this.http = axios.create({ baseURL, headers: { 'Content-Type': 'application/json' }, timeout: 30000 })
  }

  // ── Health ────────────────────────────────────────────────────────────────
  async health(): Promise<HealthResponse> {
    const { data } = await this.http.get('/health')
    return data
  }

  // ── Targets ───────────────────────────────────────────────────────────────
  async listTargets(): Promise<TargetsResponse> {
    const { data } = await this.http.get('/api/v1/targets')
    return data
  }

  async createTarget(body: Partial<Target>): Promise<{ target: Target }> {
    const { data } = await this.http.post('/api/v1/targets', body)
    return data
  }

  async updateTarget(id: string, body: Partial<Target>): Promise<{ target: Target }> {
    const { data } = await this.http.put(`/api/v1/targets/${id}`, body)
    return data
  }

  async deleteTarget(id: string): Promise<void> {
    await this.http.delete(`/api/v1/targets/${id}`)
  }

  async toggleTarget(id: string): Promise<{ id: string; status: string }> {
    const { data } = await this.http.patch(`/api/v1/targets/${id}/toggle`)
    return data
  }

  // ── Logs ──────────────────────────────────────────────────────────────────
  async getLogs(lines = 200): Promise<{ lines: string[]; total: number; log_file: string }> {
    const { data } = await this.http.get('/api/v1/logs', { params: { lines } })
    return data
  }

  // ── Tracking ──────────────────────────────────────────────────────────────
  async getTracking(params: {
    limit?: number; page?: number; search?: string; level?: string
  }): Promise<PagedResponse<TrackingRecord>> {
    const { data } = await this.http.get('/api/v1/tracking', { params })
    return data
  }

  // ── Samples ───────────────────────────────────────────────────────────────
  async getSamples(params: {
    target_id?: string; limit?: number; page?: number
  }): Promise<PagedResponse<SampleRecord>> {
    const { data } = await this.http.get('/api/v1/samples', { params })
    return data
  }

  async getSampleCollections(): Promise<{ collections: string[] }> {
    const { data } = await this.http.get('/api/v1/samples/collections')
    return data
  }

  // ── Classifications ───────────────────────────────────────────────────────
  async getClassifications(params: {
    target_id?: string; limit?: number; page?: number
  }): Promise<PagedResponse<ClassificationRecord>> {
    const { data } = await this.http.get('/api/v1/classifications', { params })
    return data
  }

  // ── Admin settings ────────────────────────────────────────────────────────
  async getAdminSettings(): Promise<SettingsResponse> {
    const { data } = await this.http.get('/api/v1/admin/settings')
    return data
  }

  async saveAdminSettings(settings: AdminSettings): Promise<{ saved: boolean; restart_required: boolean }> {
    const { data } = await this.http.put('/api/v1/admin/settings', settings)
    return data
  }

  async fetchModels(
    baseUrl: string,
    apiKey: string,
  ): Promise<{ ok: boolean; models: string[]; error?: string }> {
    const { data } = await this.http.get('/api/v1/admin/models', {
      params: { base_url: baseUrl || undefined, api_key: apiKey || undefined },
    })
    return data
  }
}

export const client = new LogflayerClient()
export default LogflayerClient
