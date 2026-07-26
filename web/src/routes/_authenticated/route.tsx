import { createFileRoute, redirect } from '@tanstack/react-router'
import { getApiToken } from '@/lib/api'
import { AuthenticatedLayout } from '@/components/layout/authenticated-layout'

export const Route = createFileRoute('/_authenticated')({
  beforeLoad: () => {
    if (!getApiToken()) throw redirect({ to: '/access' })
  },
  component: AuthenticatedLayout,
})
