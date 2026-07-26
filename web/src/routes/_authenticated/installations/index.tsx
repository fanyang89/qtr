import { createFileRoute } from '@tanstack/react-router'
import { InstallationsPage } from '@/features/installations/installations-page'

export const Route = createFileRoute('/_authenticated/installations/')({
  component: InstallationsPage,
})
