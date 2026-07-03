import { createFileRoute } from '@tanstack/react-router'
import { VmDetail } from '@/features/vms/detail'

export const Route = createFileRoute('/_authenticated/vms/$name')({
  component: VmDetailRoute,
})

function VmDetailRoute() {
  const { name } = Route.useParams()
  return <VmDetail name={name} />
}
