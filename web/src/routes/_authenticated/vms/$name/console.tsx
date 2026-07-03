import { createFileRoute } from '@tanstack/react-router'
import { ConsolePage } from '@/features/console'

export const Route = createFileRoute('/_authenticated/vms/$name/console')({
  component: ConsoleRoute,
})

function ConsoleRoute() {
  const { name } = Route.useParams()
  return <ConsolePage name={name} />
}
