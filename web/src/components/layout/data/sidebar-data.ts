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
          title: 'Automated Installs',
          url: '/installations',
          icon: ListTodo,
        },
      ],
    },
    {
      title: 'Resources',
      items: [
        {
          title: 'Disks',
          url: '/resources/disks',
          icon: HardDrive,
        },
        {
          title: 'ISOs',
          url: '/resources/isos',
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
