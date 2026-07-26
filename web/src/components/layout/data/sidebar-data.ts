import { Disc3, HardDrive, ListTodo, Server, Settings } from 'lucide-react'
import { type SidebarData } from '../types'

export const sidebarData: SidebarData = {
  navGroups: [
    {
      title: 'Workspace',
      items: [
        {
          title: 'Virtual Machines',
          url: '/vms',
          icon: Server,
        },
        {
          title: 'Installations',
          url: '/installations',
          icon: ListTodo,
        },
      ],
    },
    {
      title: 'Resources',
      items: [
        {
          title: 'Images',
          url: '/resources/images',
          icon: HardDrive,
        },
        {
          title: 'Media',
          url: '/resources/media',
          icon: Disc3,
        },
      ],
    },
    {
      title: 'System',
      items: [
        {
          title: 'Settings',
          url: '/settings',
          icon: Settings,
        },
      ],
    },
  ],
}
