import { z } from 'zod'
import axios, { type AxiosResponse } from 'axios'

export const vmStateSchema = z.enum([
  'nostate',
  'running',
  'blocked',
  'paused',
  'shutdown',
  'shutoff',
  'crashed',
  'pmsuspended',
  'unknown',
])

const vmMetricsSchema = z.object({
  cpuTimeNs: z.number(),
  memoryUsedMib: z.number(),
  memoryTotalMib: z.number(),
  txBytes: z.number(),
  rxBytes: z.number(),
  sampledAtMs: z.number(),
})

const vmIoThreadsSchema = z.object({
  count: z.number().int().positive(),
  queues: z.number().int().positive().optional(),
})

const vmDiskIoSchema = z.object({
  mode: z.enum(['threads', 'native', 'io_uring']),
})

const vmDiskSchema = z.object({
  type: z.enum(['file', 'block']).optional(),
  path: z.string(),
  format: z.enum(['raw', 'qcow2']),
  target: z.string().optional(),
  bus: z.enum(['virtio-blk', 'virtio-scsi', 'virtio']).optional(),
  cache: z
    .enum([
      'default',
      'none',
      'writethrough',
      'writeback',
      'directsync',
      'unsafe',
    ])
    .optional(),
  io: vmDiskIoSchema.optional(),
})

export const vmSummarySchema = z.object({
  name: z.string(),
  state: vmStateSchema,
  id: z.string().nullable(),
  vnc: z.boolean(),
  vncEndpoint: z.string().nullish(),
  serialLog: z.string().nullish(),
  memoryMib: z.number().nullish(),
  vcpus: z.number().nullish(),
  ioThreads: vmIoThreadsSchema.nullish(),
  network: z.string().nullish(),
  disks: z.array(vmDiskSchema).nullish(),
  cdrom: z.string().nullish(),
  boot: z.array(z.string()).nullish(),
  graphics: z.enum(['vnc', 'none']).nullish(),
  vncListen: z.string().nullish(),
  vncPort: z.number().nullish(),
  metrics: vmMetricsSchema.nullish(),
})

const vmSummaryArraySchema = z.array(vmSummarySchema)

const healthStatusSchema = z.object({
  ok: z.boolean(),
  libvirtUri: z.string(),
  version: z.string().optional(),
})

const vncTicketSchema = z.object({
  ticket: z.string(),
  expiresInSeconds: z.number().int().positive(),
})

export const jobStatusSchema = z.enum([
  'queued',
  'running',
  'succeeded',
  'failed',
  'cancelled',
  'interrupted',
])

export const fedoraInstallRequestSchema = z.object({
  name: z.string(),
  mediaId: z.string(),
  imageId: z.string(),
  sshAuthorizedKey: z.string(),
  diskSize: z.string(),
  memoryMib: z.number(),
  vcpus: z.number(),
  network: z.string(),
  hostname: z.string().nullish(),
  mirror: z.enum(['official', 'tuna']),
  timeoutSecs: z.number(),
  verifyTimeoutSecs: z.number(),
  keepFailed: z.boolean(),
})

export const installJobSchema = z.object({
  id: z.string(),
  status: jobStatusSchema,
  phase: z.string(),
  cancelRequested: z.boolean(),
  request: fedoraInstallRequestSchema,
  error: z.string().nullish(),
  createdAtMs: z.number(),
  startedAtMs: z.number().nullish(),
  finishedAtMs: z.number().nullish(),
})

export const managedResourceSchema = z.object({
  id: z.string(),
  sizeBytes: z.number(),
  modifiedAtMs: z.number().nullish(),
})

export const networkSummarySchema = z.object({
  id: z.string(),
  active: z.boolean(),
  autostart: z.boolean(),
  bridge: z.string().nullish(),
})

const installJobArraySchema = z.array(installJobSchema)
const managedResourceArraySchema = z.array(managedResourceSchema)
const networkSummaryArraySchema = z.array(networkSummarySchema)

export type VmState = z.infer<typeof vmStateSchema>
export type VmMetrics = z.infer<typeof vmMetricsSchema>
export type VmDisk = z.infer<typeof vmDiskSchema>
export type VmSummary = z.infer<typeof vmSummarySchema>
export type HealthStatus = z.infer<typeof healthStatusSchema>
export type VncTicket = z.infer<typeof vncTicketSchema>
export type JobStatus = z.infer<typeof jobStatusSchema>
export type FedoraInstallRequest = z.infer<typeof fedoraInstallRequestSchema>
export type InstallJob = z.infer<typeof installJobSchema>
export type ManagedResource = z.infer<typeof managedResourceSchema>
export type NetworkSummary = z.infer<typeof networkSummarySchema>

export type VmCreateInput = {
  name: string
  resources: {
    vcpus: number
    memoryMib: number
  }
  disks: Array<{
    imageId: string
    format: 'raw' | 'qcow2'
    bus: 'virtio-blk' | 'virtio-scsi'
  }>
  networkId: string
  mediaId: string | null
  console: {
    graphics: 'vnc' | 'none'
    serialLog: boolean
  }
}

export const API_TOKEN_STORAGE_KEY = 'qtr.apiToken'

export function getApiToken(): string {
  return sessionStorage.getItem(API_TOKEN_STORAGE_KEY) ?? ''
}

export function setApiToken(token: string): void {
  if (token) {
    sessionStorage.setItem(API_TOKEN_STORAGE_KEY, token)
  } else {
    sessionStorage.removeItem(API_TOKEN_STORAGE_KEY)
  }
}

export function bootstrapDevelopmentSession(): void {
  const token = import.meta.env.VITE_QTR_API_TOKEN
  if (import.meta.env.DEV && token && !getApiToken()) setApiToken(token)
}

const apiClient = axios.create({ baseURL: '/api/v1' })

apiClient.interceptors.request.use((config) => {
  const token = getApiToken()
  if (token) config.headers.Authorization = `Bearer ${token}`
  return config
})

apiClient.interceptors.response.use(undefined, (error: unknown) => {
  if (
    axios.isAxiosError(error) &&
    error.response?.status === 401 &&
    window.location.pathname !== '/access'
  ) {
    setApiToken('')
    window.location.assign('/access')
  }
  return Promise.reject(error)
})

async function parseResponse<T>(
  request: Promise<AxiosResponse<unknown>>,
  schema: z.ZodType<T>
): Promise<T> {
  const { data } = await request
  return schema.parse(data)
}

export async function getHealth(): Promise<HealthStatus> {
  return parseResponse(apiClient.get('/health'), healthStatusSchema)
}

export async function validateSession(): Promise<void> {
  await apiClient.get('/session')
}

export async function getVms(): Promise<VmSummary[]> {
  return parseResponse(apiClient.get('/vms'), vmSummaryArraySchema)
}

export async function getVm(name: string): Promise<VmSummary> {
  return parseResponse(
    apiClient.get(`/vms/${encodeURIComponent(name)}`),
    vmSummarySchema
  )
}

export async function postVmAction(
  name: string,
  action: string
): Promise<void> {
  await apiClient.post(`/vms/${encodeURIComponent(name)}/${action}`)
}

export async function createVm(input: VmCreateInput): Promise<VmSummary> {
  return parseResponse(apiClient.post('/vms', input), vmSummarySchema)
}

export async function deleteVm(name: string): Promise<void> {
  await apiClient.delete(`/vms/${encodeURIComponent(name)}`)
}

export async function createVncTicket(name: string): Promise<VncTicket> {
  return parseResponse(
    apiClient.post(`/vms/${encodeURIComponent(name)}/vnc-ticket`),
    vncTicketSchema
  )
}

export async function getInstallJobs(): Promise<InstallJob[]> {
  return parseResponse(apiClient.get('/install-jobs'), installJobArraySchema)
}

export async function getInstallJob(id: string): Promise<InstallJob> {
  return parseResponse(
    apiClient.get(`/install-jobs/${encodeURIComponent(id)}`),
    installJobSchema
  )
}

export async function createInstallJob(
  request: FedoraInstallRequest
): Promise<InstallJob> {
  return parseResponse(
    apiClient.post('/install-jobs', request),
    installJobSchema
  )
}

export async function cancelInstallJob(id: string): Promise<InstallJob> {
  return parseResponse(
    apiClient.post(`/install-jobs/${encodeURIComponent(id)}/cancel`),
    installJobSchema
  )
}

export async function getDisks(): Promise<ManagedResource[]> {
  return parseResponse(apiClient.get('/images'), managedResourceArraySchema)
}

export async function getIsos(): Promise<ManagedResource[]> {
  return parseResponse(apiClient.get('/media'), managedResourceArraySchema)
}

export async function getNetworks(): Promise<NetworkSummary[]> {
  return parseResponse(apiClient.get('/networks'), networkSummaryArraySchema)
}
