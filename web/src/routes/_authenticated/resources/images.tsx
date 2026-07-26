import { createFileRoute } from '@tanstack/react-router'
import { ResourceInventory } from '@/features/resources/resource-inventory'

export const Route = createFileRoute('/_authenticated/resources/images')({
  component: () => <ResourceInventory kind='images' />,
})
