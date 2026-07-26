import { createFileRoute } from '@tanstack/react-router'
import { VmDashboard } from '@/features/vms/vm-dashboard'

export const Route = createFileRoute('/_authenticated/vms/')({
  component: VmDashboard,
})
