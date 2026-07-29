import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { describe, expect, test, vi } from 'vitest'
import { render } from 'vitest-browser-react'
import { userEvent } from 'vitest/browser'
import { createDisk } from '@/lib/api'
import { CreateDiskButton } from './disk-actions'

vi.mock('@/lib/api', () => ({
  createDisk: vi.fn(async (input) => ({
    id: input.id,
    sizeBytes: input.sizeBytes,
    modifiedAtMs: 1,
  })),
}))

function wrapper(children: React.ReactNode) {
  return (
    <QueryClientProvider client={new QueryClient()}>
      {children}
    </QueryClientProvider>
  )
}

describe('disk actions', () => {
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
})
