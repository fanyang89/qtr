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

export const vmCdromSchema = z.object({
  id: z.string(),
  target: z.string(),
  mediaId: z.string().nullish(),
  sourcePath: z.string().nullish(),
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
  cdroms: z.array(vmCdromSchema).default([]),
  boot: z.array(z.string()).nullish(),
  graphics: z.enum(['vnc', 'none']).nullish(),
  vncListen: z.string().nullish(),
  vncPort: z.number().nullish(),
  metrics: vmMetricsSchema.nullish(),
})

const vmSummaryArraySchema = z.array(vmSummarySchema)

export const guestNetworkAddressSchema = z.object({
  type: z.enum(['ipv4', 'ipv6']),
  address: z.string(),
  prefix: z.number().int().nonnegative(),
  usable: z.boolean(),
})

export const guestNetworkInterfaceSchema = z.object({
  name: z.string(),
  hardwareAddress: z.string().nullish(),
  addresses: z.array(guestNetworkAddressSchema),
})

export const vmGuestStatusSchema = z.object({
  name: z.string(),
  domainState: vmStateSchema,
  guestAgentReady: z.boolean(),
  networkInterfacesAvailable: z.boolean(),
  interfaces: z.array(guestNetworkInterfaceSchema),
  observedAtMs: z.number(),
})

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
  virtualSizeBytes: z.number().nullish(),
  modifiedAtMs: z.number().nullish(),
})

export const imageAttachmentSchema = z.object({
  imageId: z.string(),
  vmName: z.string(),
  vmState: vmStateSchema,
  target: z.string(),
  active: z.boolean(),
})

export const managedImageSchema = managedResourceSchema.extend({
  format: z.enum(['raw', 'qcow2']).nullish(),
  backingImageId: z.string().nullish(),
  status: z.enum(['ready', 'invalid']),
  attachments: z.array(imageAttachmentSchema),
  reservedByJobId: z.string().nullish(),
})

export const isoAttachmentSchema = z.object({
  mediaId: z.string(),
  vmName: z.string(),
  vmState: vmStateSchema,
  trayId: z.string(),
  target: z.string(),
  live: z.boolean(),
  persistent: z.boolean(),
})

export const managedIsoSchema = z.object({
  id: z.string(),
  sizeBytes: z.number(),
  modifiedAtMs: z.number().nullish(),
  status: z.enum(['ready', 'invalid']),
  attachments: z.array(isoAttachmentSchema),
  reservedByJobIds: z.array(z.string()),
})

export const networkSummarySchema = z.object({
  id: z.string(),
  active: z.boolean(),
  autostart: z.boolean(),
  bridge: z.string().nullish(),
})

const installJobArraySchema = z.array(installJobSchema)
const managedImageArraySchema = z.array(managedImageSchema)
const managedIsoArraySchema = z.array(managedIsoSchema)
const networkSummaryArraySchema = z.array(networkSummarySchema)

export type VmState = z.infer<typeof vmStateSchema>
export type VmMetrics = z.infer<typeof vmMetricsSchema>
export type VmDisk = z.infer<typeof vmDiskSchema>
export type VmCdrom = z.infer<typeof vmCdromSchema>
export type VmSummary = z.infer<typeof vmSummarySchema>
export type GuestNetworkAddress = z.infer<typeof guestNetworkAddressSchema>
export type GuestNetworkInterface = z.infer<typeof guestNetworkInterfaceSchema>
export type VmGuestStatus = z.infer<typeof vmGuestStatusSchema>
export type HealthStatus = z.infer<typeof healthStatusSchema>
export type VncTicket = z.infer<typeof vncTicketSchema>
export type JobStatus = z.infer<typeof jobStatusSchema>
export type FedoraInstallRequest = z.infer<typeof fedoraInstallRequestSchema>
export type InstallJob = z.infer<typeof installJobSchema>
export type ManagedResource = z.infer<typeof managedResourceSchema>
export type ManagedImage = z.infer<typeof managedImageSchema>
export type ManagedIso = z.infer<typeof managedIsoSchema>
export type NetworkSummary = z.infer<typeof networkSummarySchema>

export type ImageCreateInput = {
  id: string
  format: 'raw' | 'qcow2'
  sizeBytes: number
}

const cloudInitTextEncoder = new TextEncoder()
const cloudInitFieldMaxBytes = 1024 * 1024
const cloudInitTotalMaxBytes = 2 * 1024 * 1024
const cloudInitIsoIdPattern = /^[A-Za-z0-9][A-Za-z0-9._-]*\.iso$/i
const cloudInitHostnameLabelPattern =
  /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?$/

function utf8Bytes(value: string): number {
  return cloudInitTextEncoder.encode(value).byteLength
}

export const cloudInitSeedInputSchema = z
  .object({
    id: z.string(),
    instanceId: z.string(),
    localHostname: z.string(),
    userData: z.string(),
    networkConfig: z.string().optional(),
    vendorData: z.string().optional(),
  })
  .superRefine((value, context) => {
    if (!cloudInitIsoIdPattern.test(value.id) || utf8Bytes(value.id) > 255) {
      context.addIssue({
        code: 'custom',
        path: ['id'],
        message: 'Use a safe managed media ID ending in .iso',
      })
    }
    if (
      value.instanceId.trim().length === 0 ||
      value.instanceId.includes('\0') ||
      utf8Bytes(value.instanceId) > 255
    ) {
      context.addIssue({
        code: 'custom',
        path: ['instanceId'],
        message:
          'Instance ID must be non-blank, NUL-free, and at most 255 bytes',
      })
    }
    const hostnameBytes = utf8Bytes(value.localHostname)
    if (
      hostnameBytes === 0 ||
      hostnameBytes > 253 ||
      value.localHostname
        .split('.')
        .some((label) => !cloudInitHostnameLabelPattern.test(label))
    ) {
      context.addIssue({
        code: 'custom',
        path: ['localHostname'],
        message: 'Use a valid DNS hostname of at most 253 bytes',
      })
    }
    const contentFields = [
      ['userData', value.userData],
      ['networkConfig', value.networkConfig],
      ['vendorData', value.vendorData],
    ] as const
    for (const [name, content] of contentFields) {
      if (
        content !== undefined &&
        utf8Bytes(content) > cloudInitFieldMaxBytes
      ) {
        context.addIssue({
          code: 'custom',
          path: [name],
          message: `Must not exceed ${cloudInitFieldMaxBytes} UTF-8 bytes`,
        })
      }
    }
    const totalBytes =
      utf8Bytes(value.instanceId) +
      hostnameBytes +
      utf8Bytes(value.userData) +
      utf8Bytes(value.networkConfig ?? '') +
      utf8Bytes(value.vendorData ?? '')
    if (totalBytes > cloudInitTotalMaxBytes) {
      context.addIssue({
        code: 'custom',
        message: `Cloud-init seed content must not exceed ${cloudInitTotalMaxBytes} UTF-8 bytes`,
      })
    }
  })

export type CloudInitSeedInput = z.infer<typeof cloudInitSeedInputSchema>

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
  mediaId?: string | null
  cdroms?: Array<{ id: string; mediaId: string | null }>
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

export async function getVmGuestStatus(name: string): Promise<VmGuestStatus> {
  return parseResponse(
    apiClient.get(`/vms/${encodeURIComponent(name)}/guest-status`),
    vmGuestStatusSchema
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

export async function getDisks(): Promise<ManagedImage[]> {
  return parseResponse(apiClient.get('/images'), managedImageArraySchema)
}

export async function createDisk(
  input: ImageCreateInput
): Promise<ManagedImage> {
  return parseResponse(apiClient.post('/images', input), managedImageSchema)
}

export async function cloneDisk(
  backingImageId: string,
  id: string
): Promise<ManagedImage> {
  return parseResponse(
    apiClient.post(`/images/${encodeURIComponent(backingImageId)}/clone`, {
      id,
    }),
    managedImageSchema
  )
}

export async function resizeDisk(
  id: string,
  sizeBytes: number
): Promise<ManagedImage> {
  return parseResponse(
    apiClient.post(`/images/${encodeURIComponent(id)}/resize`, { sizeBytes }),
    managedImageSchema
  )
}

export async function deleteDisk(id: string): Promise<void> {
  await apiClient.delete(`/images/${encodeURIComponent(id)}`)
}

export async function attachDisk(
  name: string,
  imageId: string,
  bus: 'virtio-blk' | 'virtio-scsi'
): Promise<VmSummary> {
  return parseResponse(
    apiClient.put(
      `/vms/${encodeURIComponent(name)}/disks/${encodeURIComponent(imageId)}`,
      { bus }
    ),
    vmSummarySchema
  )
}

export async function detachDisk(
  name: string,
  imageId: string
): Promise<VmSummary> {
  return parseResponse(
    apiClient.delete(
      `/vms/${encodeURIComponent(name)}/disks/${encodeURIComponent(imageId)}`
    ),
    vmSummarySchema
  )
}

export async function getIsos(): Promise<ManagedIso[]> {
  return parseResponse(apiClient.get('/media'), managedIsoArraySchema)
}

export async function createCloudInitSeed(
  input: CloudInitSeedInput
): Promise<ManagedIso> {
  const request = cloudInitSeedInputSchema.parse(input)
  return parseResponse(
    apiClient.post('/media/cloud-init', request),
    managedIsoSchema
  )
}

export async function uploadDiskImage(
  id: string,
  file: File,
  options: {
    signal?: AbortSignal
    onProgress?: (loaded: number, total: number) => void
  } = {}
): Promise<ManagedImage> {
  return parseResponse(
    apiClient.put(`/images/${encodeURIComponent(id)}`, file, {
      headers: { 'Content-Type': 'application/octet-stream' },
      signal: options.signal,
      onUploadProgress: (event) =>
        options.onProgress?.(event.loaded, file.size),
    }),
    managedImageSchema
  )
}

export async function uploadIso(
  id: string,
  file: File,
  options: {
    signal?: AbortSignal
    onProgress?: (loaded: number, total: number) => void
  } = {}
): Promise<ManagedIso> {
  return parseResponse(
    apiClient.put(`/media/${encodeURIComponent(id)}`, file, {
      headers: { 'Content-Type': 'application/octet-stream' },
      signal: options.signal,
      onUploadProgress: (event) =>
        options.onProgress?.(event.loaded, file.size),
    }),
    managedIsoSchema
  )
}

export async function deleteIso(id: string): Promise<void> {
  await apiClient.delete(`/media/${encodeURIComponent(id)}`)
}

export async function addCdromTray(
  name: string,
  id: string,
  mediaId: string | null
): Promise<VmSummary> {
  return parseResponse(
    apiClient.post(`/vms/${encodeURIComponent(name)}/cdroms`, { id, mediaId }),
    vmSummarySchema
  )
}

export async function setCdromMedia(
  name: string,
  trayId: string,
  mediaId: string
): Promise<VmSummary> {
  return parseResponse(
    apiClient.put(
      `/vms/${encodeURIComponent(name)}/cdroms/${encodeURIComponent(trayId)}/media`,
      { mediaId }
    ),
    vmSummarySchema
  )
}

export async function ejectCdromMedia(
  name: string,
  trayId: string
): Promise<VmSummary> {
  return parseResponse(
    apiClient.delete(
      `/vms/${encodeURIComponent(name)}/cdroms/${encodeURIComponent(trayId)}/media`
    ),
    vmSummarySchema
  )
}

export async function removeCdromTray(
  name: string,
  trayId: string
): Promise<VmSummary> {
  return parseResponse(
    apiClient.delete(
      `/vms/${encodeURIComponent(name)}/cdroms/${encodeURIComponent(trayId)}`
    ),
    vmSummarySchema
  )
}

export async function getNetworks(): Promise<NetworkSummary[]> {
  return parseResponse(apiClient.get('/networks'), networkSummaryArraySchema)
}
