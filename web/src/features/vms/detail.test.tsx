import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { beforeEach, describe, expect, test, vi } from 'vitest'
import { render } from 'vitest-browser-react'
import { userEvent } from 'vitest/browser'
import {
  attachDisk,
  detachDisk,
  getDisks,
  type ManagedImage,
  type VmDisk,
} from '@/lib/api'
import { VmStorage } from './detail'

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

vi.mock('@/lib/api', () => ({
  getVm: vi.fn(),
  getDisks: vi.fn(async () => [managedDisk]),
  attachDisk: vi.fn(async () => undefined),
  detachDisk: vi.fn(async () => undefined),
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
