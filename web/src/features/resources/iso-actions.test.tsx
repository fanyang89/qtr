import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { describe, expect, test, vi } from 'vitest'
import { render } from 'vitest-browser-react'
import { page, userEvent } from 'vitest/browser'
import { deleteIso, uploadIso } from '@/lib/api'
import { DeleteIsoButton, IsoUploadButton } from './iso-actions'

vi.mock('@/lib/api', () => ({
  uploadIso: vi.fn(async (id: string, file: File) => ({
    id,
    sizeBytes: file.size,
    modifiedAtMs: 1,
  })),
  deleteIso: vi.fn(async () => undefined),
}))

function wrapper(children: React.ReactNode) {
  return (
    <QueryClientProvider client={new QueryClient()}>
      {children}
    </QueryClientProvider>
  )
}

describe('ISO actions', () => {
  test('selects and uploads an ISO with a sanitized ID', async () => {
    const screen = await render(wrapper(<IsoUploadButton />))
    await userEvent.click(screen.getByRole('button', { name: 'Upload ISO' }))
    const dialog = screen.getByRole('dialog')
    const input = document.querySelector<HTMLInputElement>('#iso-file')!
    const transfer = new DataTransfer()
    transfer.items.add(new File(['iso'], 'Fedora Server.iso'))
    input.files = transfer.files
    input.dispatchEvent(new Event('change', { bubbles: true }))

    await expect
      .element(dialog.getByLabelText('ISO ID'))
      .toHaveValue('Fedora-Server.iso')
    await userEvent.click(dialog.getByRole('button', { name: 'Upload ISO' }))

    expect(vi.mocked(uploadIso)).toHaveBeenCalledWith(
      'Fedora-Server.iso',
      expect.any(File),
      expect.objectContaining({ signal: expect.any(AbortSignal) })
    )
  })

  test('confirms ISO deletion', async () => {
    const screen = await render(
      wrapper(
        <DeleteIsoButton
          iso={{ id: 'Fedora.iso', sizeBytes: 3, modifiedAtMs: 1 }}
        />
      )
    )
    await userEvent.click(
      screen.getByRole('button', { name: 'Delete Fedora.iso' })
    )
    await expect
      .element(page.getByRole('heading', { name: 'Delete Fedora.iso?' }))
      .toBeVisible()
    await userEvent.click(
      page.getByRole('button', { name: 'Delete', exact: true })
    )

    expect(vi.mocked(deleteIso)).toHaveBeenCalledWith('Fedora.iso')
  })
})
