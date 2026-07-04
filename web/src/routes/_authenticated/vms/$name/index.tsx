/* eslint-disable react-refresh/only-export-components */
import { createFileRoute } from '@tanstack/react-router'
import { VmDetail } from '@/features/vms/detail'

function VmDetailRoute() {
  const { name } = Route.useParams()
  return <VmDetail name={name} />
}

export const Route = createFileRoute('/_authenticated/vms/$name/')({
  component: VmDetailRoute,
})
