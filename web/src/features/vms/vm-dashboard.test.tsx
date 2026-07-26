import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { describe, expect, test, vi } from 'vitest'
import { render } from 'vitest-browser-react'
import { VmDashboard } from './vm-dashboard'

vi.mock('@/lib/api', () => ({
  createVm: vi.fn(),
  deleteVm: vi.fn(),
  getVms: vi.fn(() => new Promise(() => {})),
  postVmAction: vi.fn(),
  updateVm: vi.fn(),
}))

vi.mock('@/components/layout/header', () => ({
  Header: ({ children }: { children: React.ReactNode }) => (
    <header>{children}</header>
  ),
}))
vi.mock('@/components/layout/main', () => ({
  Main: ({ children }: { children: React.ReactNode }) => (
    <main>{children}</main>
  ),
}))
vi.mock('@/components/search', () => ({ Search: () => null }))
vi.mock('@/components/theme-switch', () => ({ ThemeSwitch: () => null }))
vi.mock('./vm-form-dialog', () => ({ VmFormDialog: () => null }))

describe('VmDashboard', () => {
  test('does not loop while the initial VM query is pending', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {})
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    })

    await render(
      <QueryClientProvider client={queryClient}>
        <VmDashboard />
      </QueryClientProvider>
    )
    await new Promise((resolve) => requestAnimationFrame(resolve))

    expect(consoleError.mock.calls.flat().join(' ')).not.toContain(
      'Maximum update depth exceeded'
    )
    consoleError.mockRestore()
  })
})
