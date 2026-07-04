import { z } from 'zod'
import axios, { type AxiosResponse } from 'axios'

const vmStateSchema = z.enum([
  'running',
  'shutoff',
  'paused',
  'blocked',
  'crashed',
  'shutdown',
  'unknown',
])

const vmMetricsSchema = z.object({
  cpuTimeNs: z.number(),
  memoryUsedMiB: z.number(),
  memoryTotalMiB: z.number(),
  txBytes: z.number(),
  rxBytes: z.number(),
  sampledAtMs: z.number(),
})

const vmDiskSchema = z.object({
  type: z.enum(['file', 'block']).optional(),
  path: z.string(),
  format: z.enum(['raw', 'qcow2']),
  target: z.string().optional(),
  bus: z.string().optional(),
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
  io: z.enum(['threads', 'native', 'io_uring']).optional(),
})

const vmSummarySchema = z.object({
  name: z.string(),
  state: vmStateSchema,
  id: z.string().nullable(),
  vnc: z.boolean(),
  vncEndpoint: z.string().nullish(),
  serialLog: z.string().nullish(),
  memoryMiB: z.number().nullish(),
  vcpus: z.number().nullish(),
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

export type VmState = z.infer<typeof vmStateSchema>
export type VmMetrics = z.infer<typeof vmMetricsSchema>
export type VmDisk = z.infer<typeof vmDiskSchema>
export type VmSummary = z.infer<typeof vmSummarySchema>
export type HealthStatus = z.infer<typeof healthStatusSchema>

export type VmCreateInput = {
  name: string
  disks: VmDisk[]
  cdrom?: string
  boot?: string[]
  memoryGiB: number
  vcpus: number
  network: string
  graphics: 'vnc' | 'none'
  vncListen: string
  vncPort?: number
  serialLog?: string
}

export type VmUpdateInput = VmCreateInput

const apiClient = axios.create({ baseURL: '/api' })

async function parseResponse<T>(
  request: Promise<AxiosResponse<unknown>>,
  schema: z.ZodType<T>
): Promise<T> {
  const { data } = await request
  return schema.parse(data)
}

export async function getHealth(): Promise<HealthStatus> {
  try {
    return await parseResponse(apiClient.get('/health'), healthStatusSchema)
  } catch {
    return { ok: false, libvirtUri: 'qemu:///system' }
  }
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

export async function updateVm(
  name: string,
  input: VmUpdateInput
): Promise<VmSummary> {
  return parseResponse(
    apiClient.put(`/vms/${encodeURIComponent(name)}`, input),
    vmSummarySchema
  )
}

export async function deleteVm(name: string): Promise<void> {
  await apiClient.delete(`/vms/${encodeURIComponent(name)}`)
}
