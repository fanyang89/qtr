import { createFileRoute } from '@tanstack/react-router'
import { VmList } from '@/features/vms'

export const Route = createFileRoute('/_authenticated/vms/')({
  component: VmList,
})
