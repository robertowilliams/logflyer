import axios, { type AxiosInstance } from 'axios'
import type {
  Target, SampleRecord, TrackingRecord, HealthResponse,
  PagedResponse, TargetsResponse, ClassificationRecord,
  AdminSettings, SettingsResponse, HistoryEntry,
  SampleMetadata, DeletionRecord, EntityRecord,
  GraphTraversal, GraphPath,
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

  async deleteSample(hash: string, targetId: string, reason: string): Promise<void> {
    await this.http.delete(`/api/v1/samples/${hash}`, {
      data: { target_id: targetId, reason },
    })
  }

  // ── Deletions audit log ───────────────────────────────────────────────────
  async getDeletions(params: {
    limit?: number; page?: number
  }): Promise<PagedResponse<DeletionRecord>> {
    const { data } = await this.http.get('/api/v1/sample-deletions', { params })
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

  async getSettingsHistory(): Promise<{ entries: HistoryEntry[] }> {
    const { data } = await this.http.get('/api/v1/admin/settings/history')
    return data
  }

  async restoreSettingsVersion(version: number): Promise<{ restored: boolean; version: number }> {
    const { data } = await this.http.post(`/api/v1/admin/settings/restore/${version}`)
    return data
  }

  async restartService(): Promise<{ accepted: boolean; message: string }> {
    const { data } = await this.http.post('/api/v1/admin/restart')
    return data
  }

  // ── UpsideGate metadata ───────────────────────────────────────────────────
  async getMetadata(params: {
    target_id?: string; limit?: number; page?: number;
    entity_type?: string; worth_classifying?: boolean
  }): Promise<PagedResponse<SampleMetadata>> {
    const { data } = await this.http.get('/api/v1/metadata', { params })
    return data
  }

  async getMetadataByHash(sampleHash: string): Promise<{ metadata: SampleMetadata }> {
    const { data } = await this.http.get(`/api/v1/metadata/${sampleHash}`)
    return data
  }

  // ── UpsideGate output reads (Phase 6) ─────────────────────────────────────
  // Backed by the dedicated graph + vector collections written by the
  // pipeline's async output adapters.  The store may opt in to these when
  // GRAPH_WRITER_ENABLED=true; otherwise it falls back to client-side
  // synthesis from `metadata.entities` + `metadata.relations`.
  async getRelations(params: {
    sample_hash?: string; relation_type?: string;
    limit?: number; page?: number
  }): Promise<PagedResponse<unknown>> {
    const { data } = await this.http.get('/api/v1/relations', { params })
    return data
  }

  async getProvTriples(params: {
    sample_hash?: string; subject?: string; predicate?: string;
    limit?: number; page?: number
  }): Promise<PagedResponse<unknown>> {
    const { data } = await this.http.get('/api/v1/prov', { params })
    return data
  }

  async getOtelSpans(params: {
    sample_hash?: string; trace_id?: string;
    limit?: number; page?: number
  }): Promise<PagedResponse<unknown>> {
    const { data } = await this.http.get('/api/v1/spans', { params })
    return data
  }

  // ── Entity lookup ─────────────────────────────────────────────────────────
  // Resolves an entity that is not in the currently-loaded sample's
  // `metadata.entities` array — which happens as soon as a relation or PROV
  // triple points across samples.  Accepts a bare entity_id or the
  // `ug:entity:{id}` URI form; the backend strips the prefix.
  async getEntity(entityId: string): Promise<{ entity: EntityRecord }> {
    const { data } = await this.http.get(`/api/v1/entities/${encodeURIComponent(entityId)}`)
    return data
  }

  // ── Graph traversal ───────────────────────────────────────────────────────
  // Server-side BFS over the `entity_edges` collection. Unlike /relations,
  // which is scoped to one sample, these follow edges wherever they lead, so
  // the graph view can expand beyond the selected sample.
  async getDownstream(entityId: string, depth = 2): Promise<GraphTraversal> {
    const { data } = await this.http.get(
      `/api/v1/graph/downstream/${encodeURIComponent(entityId)}`,
      { params: { depth } },
    )
    return data
  }

  async getUpstream(entityId: string, depth = 2): Promise<GraphTraversal> {
    const { data } = await this.http.get(
      `/api/v1/graph/upstream/${encodeURIComponent(entityId)}`,
      { params: { depth } },
    )
    return data
  }

  async getGraphPath(from: string, to: string, maxDepth = 6): Promise<GraphPath> {
    const { data } = await this.http.get('/api/v1/graph/path', {
      params: { from, to, max_depth: maxDepth },
    })
    return data
  }

  async confirmSettings(): Promise<void> {
    await this.http.post('/api/v1/admin/confirm')
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
