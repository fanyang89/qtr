import { createFileRoute, redirect } from '@tanstack/react-router'

export const Route = createFileRoute('/_authenticated/vms/')({
  beforeLoad: () => {
    throw redirect({ to: '/' })
  },
})
