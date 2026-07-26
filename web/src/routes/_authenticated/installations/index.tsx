import { z } from 'zod'
/* eslint-disable react-refresh/only-export-components */
import { createFileRoute } from '@tanstack/react-router'
import { InstallationsPage } from '@/features/installations/installations-page'

export const Route = createFileRoute('/_authenticated/installations/')({
  validateSearch: z.object({
    create: z.boolean().optional(),
  }),
  component: InstallationsRoute,
})

function InstallationsRoute() {
  const search = Route.useSearch()
  return <InstallationsPage initialCreateOpen={search.create === true} />
}
