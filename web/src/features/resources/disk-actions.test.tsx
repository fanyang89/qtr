import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { beforeEach, describe, expect, test, vi } from 'vitest'
import { render } from 'vitest-browser-react'
import { userEvent } from 'vitest/browser'
import {
  createDisk,
  deleteDisk,
  resizeDisk,
  type ManagedImage,
} from '@/lib/api'
import {
  CreateDiskButton,
  DeleteDiskButton,
  ResizeDiskButton,
} from './disk-actions'

vi.mock('@/lib/api', () => ({
  createDisk: vi.fn(async (input) => ({
    id: input.id,
    sizeBytes: input.sizeBytes,
    virtualSizeBytes: input.sizeBytes,
    modifiedAtMs: 1,
    format: input.format,
    status: 'ready',
    attachments: [],
    reservedByJobId: null,
  })),
  resizeDisk: vi.fn(async () => undefined),
  deleteDisk: vi.fn(async () => undefined),
}))

const disk: ManagedImage = {
  id: 'data.qcow2',
  sizeBytes: 196_608,
  virtualSizeBytes: 8 * 1024 ** 3,
  modifiedAtMs: 1,
  format: 'qcow2',
  status: 'ready',
  attachments: [],
  reservedByJobId: null,
}

function wrapper(children: React.ReactNode) {
  return (
    <QueryClientProvider client={new QueryClient()}>
      {children}
    </QueryClientProvider>
  )
}

describe('disk actions', () => {
  beforeEach(() => vi.clearAllMocks())

  test('creates a managed qcow2 disk', async () => {
    const screen = await render(wrapper(<CreateDiskButton />))
    await userEvent.click(screen.getByRole('button', { name: 'Create Disk' }))
    const dialog = screen.getByRole('dialog')
    await userEvent.fill(dialog.getByLabelText('Disk ID'), 'data.qcow2')
    await userEvent.fill(dialog.getByLabelText('Size GiB'), '8')
    await userEvent.click(dialog.getByRole('button', { name: 'Create Disk' }))

    expect(vi.mocked(createDisk)).toHaveBeenCalledWith({
      id: 'data.qcow2',
      format: 'qcow2',
      sizeBytes: 8 * 1024 ** 3,
    })
  })

  test('expands a managed disk', async () => {
    const screen = await render(wrapper(<ResizeDiskButton disk={disk} />))
    await userEvent.click(
      screen.getByRole('button', { name: 'Expand data.qcow2' })
    )
    const dialog = screen.getByRole('dialog')
    await userEvent.fill(dialog.getByLabelText('New size GiB'), '12')
    await userEvent.click(dialog.getByRole('button', { name: 'Expand Disk' }))

    expect(vi.mocked(resizeDisk)).toHaveBeenCalledWith(
      'data.qcow2',
      12 * 1024 ** 3
    )
  })

  test('deletes an unattached managed disk after confirmation', async () => {
    const screen = await render(wrapper(<DeleteDiskButton disk={disk} />))
    await userEvent.click(
      screen.getByRole('button', { name: 'Delete data.qcow2' })
    )
    await userEvent.click(screen.getByRole('button', { name: 'Delete' }))

    expect(vi.mocked(deleteDisk)).toHaveBeenCalledWith('data.qcow2')
  })
})
