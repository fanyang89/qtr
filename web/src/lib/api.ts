export type VmState =
  | 'running'
  | 'shutoff'
  | 'paused'
  | 'blocked'
  | 'crashed'
  | 'shutdown'
  | 'unknown'

export type VmSummary = {
  name: string
  state: VmState
  id: string | null
  vnc: boolean
  vncEndpoint?: string
  serialLog?: string
  memoryMiB?: number
  vcpus?: number
  network?: string
  systemDisk?: string
  cdrom?: string
  boot?: string[]
  graphics?: 'vnc' | 'none'
  vncListen?: string
  vncPort?: number
}

export type VmCreateInput = {
  name: string
  systemDisk: string
  createSystemDisk?: string
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

export type VmUpdateInput = Omit<VmCreateInput, 'createSystemDisk'>

export type HealthStatus = {
  ok: boolean
  libvirtUri: string
  version?: string
}

const sampleVms: VmSummary[] = [
  {
    name: 'install-os',
    state: 'running',
    id: '3',
    vnc: true,
    vncEndpoint: '127.0.0.1:5900',
    serialLog: '.tmp/logs/install-os.serial.log',
    memoryMiB: 4096,
    vcpus: 2,
    network: 'default',
  },
  {
    name: 'smoke-fedora',
    state: 'shutoff',
    id: null,
    vnc: false,
    serialLog: '.tmp/logs/smoke-fedora.serial.log',
    memoryMiB: 2048,
    vcpus: 2,
    network: 'default',
  },
]

async function requestJson<T>(path: string, options?: RequestInit): Promise<T> {
  const response = await fetch(path, options)
  if (!response.ok) {
    const text = await response.text().catch(() => 'Request failed')
    throw new Error(text)
  }
  return response.json() as Promise<T>
}

export async function getHealth(): Promise<HealthStatus> {
  try {
    return await requestJson<HealthStatus>('/api/health')
  } catch {
    return { ok: false, libvirtUri: 'qemu:///system' }
  }
}

export async function getVms(): Promise<VmSummary[]> {
  try {
    return await requestJson<VmSummary[]>('/api/vms')
  } catch {
    return sampleVms
  }
}

export async function getVm(name: string): Promise<VmSummary> {
  try {
    return await requestJson<VmSummary>(`/api/vms/${encodeURIComponent(name)}`)
  } catch {
    return (
      sampleVms.find((vm) => vm.name === name) ?? {
        name,
        state: 'unknown',
        id: null,
        vnc: false,
      }
    )
  }
}

export async function postVmAction(name: string, action: string): Promise<void> {
  const response = await fetch(`/api/vms/${encodeURIComponent(name)}/${action}`, {
    method: 'POST',
  })
  if (!response.ok) {
    const text = await response.text().catch(() => 'VM action failed')
    throw new Error(text)
  }
}

export async function createVm(input: VmCreateInput): Promise<VmSummary> {
  return requestJson<VmSummary>('/api/vms', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(input),
  })
}

export async function updateVm(name: string, input: VmUpdateInput): Promise<VmSummary> {
  return requestJson<VmSummary>(`/api/vms/${encodeURIComponent(name)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(input),
  })
}

export async function deleteVm(name: string): Promise<void> {
  const response = await fetch(`/api/vms/${encodeURIComponent(name)}`, {
    method: 'DELETE',
  })
  if (!response.ok) {
    const text = await response.text().catch(() => 'Failed to delete VM')
    throw new Error(text)
  }
}
