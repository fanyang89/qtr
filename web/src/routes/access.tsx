import { createFileRoute } from '@tanstack/react-router'
import { AccessPage } from '@/features/auth/access-page'

export const Route = createFileRoute('/access')({
  component: AccessPage,
})
