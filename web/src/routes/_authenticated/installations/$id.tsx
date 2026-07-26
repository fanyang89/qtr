/* eslint-disable react-refresh/only-export-components */
import { createFileRoute } from '@tanstack/react-router'
import { InstallationDetail } from '@/features/installations/installation-detail'

export const Route = createFileRoute('/_authenticated/installations/$id')({
  component: InstallationRoute,
})

function InstallationRoute() {
  const { id } = Route.useParams()
  return <InstallationDetail id={id} />
}
