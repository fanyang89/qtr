/* eslint-disable react-refresh/only-export-components */
import { createFileRoute } from '@tanstack/react-router'
import { ConsolePage } from '@/features/console'

function ConsoleRoute() {
  const { name } = Route.useParams()
  return <ConsolePage name={name} />
}

export const Route = createFileRoute('/_authenticated/vms/$name/console')({
  component: ConsoleRoute,
})
