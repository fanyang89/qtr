import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { beforeEach, describe, expect, test, vi } from 'vitest'
import { render } from 'vitest-browser-react'
import { userEvent } from 'vitest/browser'
import {
  addCdromTray,
  attachDisk,
  detachDisk,
  ejectCdromMedia,
  getDisks,
  getIsos,
  removeCdromTray,
  setCdromMedia,
  type ManagedImage,
  type ManagedIso,
  type VmCdrom,
  type VmDisk,
} from '@/lib/api'
import { VmCdroms, VmStorage } from './detail'

const managedDisk: ManagedImage = {
  id: 'data.qcow2',
  sizeBytes: 196_608,
  virtualSizeBytes: 8 * 1024 ** 3,
  modifiedAtMs: 1,
  format: 'qcow2',
  status: 'ready',
  attachments: [],
  reservedByJobId: null,
}

const rootDisk: VmDisk = {
  type: 'file',
  path: '/images/root.qcow2',
  format: 'qcow2',
  target: 'vda',
  bus: 'virtio-blk',
}

const managedIso: ManagedIso = {
  id: 'tools.iso',
  sizeBytes: 32_774,
  modifiedAtMs: 1,
  status: 'ready',
  attachments: [],
  reservedByJobIds: [],
}

const loadedTray: VmCdrom = {
  id: 'installer',
  target: 'sda',
  mediaId: 'installer.iso',
  sourcePath: '/isos/installer.iso',
}

vi.mock('@/lib/api', () => ({
  getVm: vi.fn(),
  getDisks: vi.fn(async () => [managedDisk]),
  getIsos: vi.fn(async () => [managedIso]),
  attachDisk: vi.fn(async () => undefined),
  detachDisk: vi.fn(async () => undefined),
  addCdromTray: vi.fn(async () => ({ name: 'fedora', cdroms: [] })),
  setCdromMedia: vi.fn(async () => ({ name: 'fedora', cdroms: [] })),
  ejectCdromMedia: vi.fn(async () => ({ name: 'fedora', cdroms: [] })),
  removeCdromTray: vi.fn(async () => ({ name: 'fedora', cdroms: [] })),
}))

function wrapper(children: React.ReactNode) {
  return (
    <QueryClientProvider client={new QueryClient()}>
      {children}
    </QueryClientProvider>
  )
}

describe('VM storage management', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(getDisks).mockResolvedValue([managedDisk])
    vi.mocked(getIsos).mockResolvedValue([managedIso])
  })

  test('attaches an available disk to a powered-off VM', async () => {
    const screen = await render(
      wrapper(<VmStorage name='fedora' state='shutoff' disks={[rootDisk]} />)
    )
    await expect
      .element(screen.getByRole('button', { name: 'Attach Disk' }))
      .toBeEnabled()
    await userEvent.click(screen.getByRole('button', { name: 'Attach Disk' }))
    const dialog = screen.getByRole('dialog')
    await userEvent.click(dialog.getByRole('combobox').first())
    await userEvent.click(screen.getByRole('option', { name: /data.qcow2/ }))
    await userEvent.click(dialog.getByRole('button', { name: 'Attach Disk' }))

    expect(vi.mocked(attachDisk)).toHaveBeenCalledWith(
      'fedora',
      'data.qcow2',
      'virtio-blk'
    )
  })

  test('keeps attach disabled while the VM is running', async () => {
    const screen = await render(
      wrapper(<VmStorage name='fedora' state='running' disks={[rootDisk]} />)
    )

    await expect
      .element(screen.getByRole('button', { name: 'Attach Disk' }))
      .toBeDisabled()
    expect(vi.mocked(detachDisk)).not.toHaveBeenCalled()
  })
})

describe('VM optical media management', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(getIsos).mockResolvedValue([managedIso])
  })

  test('changes and ejects media while the VM is running', async () => {
    const screen = await render(
      wrapper(<VmCdroms name='fedora' state='running' cdroms={[loadedTray]} />)
    )
    await expect
      .element(screen.getByRole('button', { name: 'Add Tray' }))
      .toBeDisabled()
    await userEvent.click(screen.getByRole('button', { name: 'Change ISO' }))
    const dialog = screen.getByRole('dialog')
    await userEvent.click(dialog.getByRole('combobox'))
    await userEvent.click(screen.getByRole('option', { name: 'tools.iso' }))
    await userEvent.click(dialog.getByRole('button', { name: 'Load ISO' }))
    expect(vi.mocked(setCdromMedia)).toHaveBeenCalledWith(
      'fedora',
      'installer',
      'tools.iso'
    )

    await userEvent.click(screen.getByRole('button', { name: 'Eject' }))
    await userEvent.click(screen.getByRole('button', { name: 'Eject' }).last())
    expect(vi.mocked(ejectCdromMedia)).toHaveBeenCalledWith(
      'fedora',
      'installer'
    )
  })

  test('adds and removes trays only while shut off', async () => {
    const screen = await render(
      wrapper(<VmCdroms name='fedora' state='shutoff' cdroms={[loadedTray]} />)
    )
    await userEvent.click(screen.getByRole('button', { name: 'Add Tray' }))
    const dialog = screen.getByRole('dialog')
    await userEvent.fill(dialog.getByLabelText('Tray ID'), 'drivers')
    await userEvent.click(dialog.getByRole('button', { name: 'Add Tray' }))
    expect(vi.mocked(addCdromTray)).toHaveBeenCalledWith(
      'fedora',
      'drivers',
      null
    )

    await userEvent.click(screen.getByRole('button', { name: 'Remove' }))
    await userEvent.click(screen.getByRole('button', { name: 'Remove' }).last())
    expect(vi.mocked(removeCdromTray)).toHaveBeenCalledWith(
      'fedora',
      'installer'
    )
  })
})
