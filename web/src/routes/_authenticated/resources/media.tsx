import { createFileRoute } from '@tanstack/react-router'
import { ResourceInventory } from '@/features/resources/resource-inventory'

export const Route = createFileRoute('/_authenticated/resources/media')({
  component: () => <ResourceInventory kind='media' />,
})
