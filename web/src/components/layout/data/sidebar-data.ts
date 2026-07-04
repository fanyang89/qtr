import {
  Activity,
  Boxes,
  HardDrive,
  Settings,
  Server,
  TerminalSquare,
} from 'lucide-react'
import { type SidebarData } from '../types'

export const sidebarData: SidebarData = {
  user: {
    name: 'qtr host',
    email: 'qemu:///system',
    avatar: '',
  },
  teams: [
    {
      name: 'qtr',
      logo: TerminalSquare,
      plan: 'QEMU VM manager',
    },
  ],
  navGroups: [
    {
      title: 'Control Plane',
      items: [
        {
          title: 'Virtual Machines',
          url: '/',
          icon: Server,
        },
      ],
    },
    {
      title: 'Resources',
      items: [
        {
          title: 'Images',
          url: '/settings',
          icon: HardDrive,
          badge: 'soon',
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
        {
          title: 'Host Status',
          url: '/settings',
          icon: Activity,
        },
        {
          title: 'Networks',
          url: '/settings',
          icon: Boxes,
          badge: 'soon',
        },
      ],
    },
  ],
}
