import { Outlet, createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/_authenticated/vms/$name')({
  component: Outlet,
})
