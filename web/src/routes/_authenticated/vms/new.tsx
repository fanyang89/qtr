import { createFileRoute } from '@tanstack/react-router'
import { CreateVmPage } from '@/features/vms/create-vm-page'

export const Route = createFileRoute('/_authenticated/vms/new')({
  component: CreateVmPage,
})
