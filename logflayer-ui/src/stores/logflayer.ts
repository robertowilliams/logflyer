import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import { client } from '../api/client'
import type { Target, SampleRecord, TrackingRecord, HealthResponse, ClassificationRecord } from '../types'

export const useLogflayerStore = defineStore('logflayer', () => {
  const health = ref<HealthResponse | null>(null)
  const targets = ref<Target[]>([])
  const logLines = ref<string[]>([])
  const logFile = ref<string>('')
  const trackingRecords = ref<TrackingRecord[]>([])
  const trackingTotal = ref(0)
  const samples = ref<SampleRecord[]>([])
  const samplesTotal = ref(0)
  const sampleCollections = ref<string[]>([])
  const classifications = ref<ClassificationRecord[]>([])
  const classificationsTotal = ref(0)
  const loading = ref(false)
  const error = ref<string | null>(null)

  const isHealthy = computed(() => health.value?.status === 'healthy')
  const activeTargets = computed(() => targets.value.filter(t => t.status === 'active'))
  const inactiveTargets = computed(() => targets.value.filter(t => t.status !== 'active'))

  async function checkHealth() {
    try { health.value = await client.health() } catch { health.value = null }
  }

  async function fetchTargets() {
    try {
      loading.value = true; error.value = null
      const res = await client.listTargets()
      targets.value = res.targets
    } catch (e: any) {
      error.value = e.message
    } finally { loading.value = false }
  }

  async function createTarget(body: Partial<Target>) {
    const res = await client.createTarget(body)
    targets.value.unshift(res.target)
    return res.target
  }

  async function updateTarget(id: string, body: Partial<Target>) {
    const res = await client.updateTarget(id, body)
    const idx = targets.value.findIndex(t => t.id === id)
    if (idx !== -1) targets.value[idx] = res.target
    return res.target
  }

  async function deleteTarget(id: string) {
    await client.deleteTarget(id)
    targets.value = targets.value.filter(t => t.id !== id)
  }

  async function toggleTarget(id: string) {
    const res = await client.toggleTarget(id)
    const idx = targets.value.findIndex(t => t.id === id)
    if (idx !== -1) targets.value[idx]!.status = res.status
  }

  async function fetchLogs(lines = 200) {
    try {
      loading.value = true; error.value = null
      const res = await client.getLogs(lines)
      logLines.value = res.lines
      logFile.value = res.log_file
    } catch (e: any) {
      error.value = e.message
    } finally { loading.value = false }
  }

  async function fetchTracking(params: { limit?: number; page?: number; search?: string; level?: string }) {
    try {
      loading.value = true; error.value = null
      const res = await client.getTracking(params)
      trackingRecords.value = res.records
      trackingTotal.value = res.total
    } catch (e: any) {
      error.value = e.message
    } finally { loading.value = false }
  }

  async function fetchSamples(params: { target_id?: string; limit?: number; page?: number }) {
    try {
      loading.value = true; error.value = null
      const res = await client.getSamples(params)
      samples.value = res.records
      samplesTotal.value = res.total
    } catch (e: any) {
      error.value = e.message
    } finally { loading.value = false }
  }

  async function fetchSampleCollections() {
    try {
      const res = await client.getSampleCollections()
      sampleCollections.value = res.collections
    } catch { sampleCollections.value = [] }
  }

  async function fetchClassifications(params: { target_id?: string; limit?: number; page?: number }) {
    try {
      loading.value = true; error.value = null
      const res = await client.getClassifications(params)
      classifications.value = res.records
      classificationsTotal.value = res.total
    } catch (e: any) {
      error.value = e.message
    } finally { loading.value = false }
  }

  function clearError() { error.value = null }

  return {
    health, targets, logLines, logFile,
    trackingRecords, trackingTotal,
    samples, samplesTotal, sampleCollections,
    classifications, classificationsTotal,
    loading, error,
    isHealthy, activeTargets, inactiveTargets,
    checkHealth, fetchTargets, createTarget, updateTarget, deleteTarget, toggleTarget,
    fetchLogs, fetchTracking, fetchSamples, fetchSampleCollections,
    fetchClassifications,
    clearError,
  }
})
