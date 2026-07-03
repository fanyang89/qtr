import {
  Activity,
  Boxes,
  CirclePlay,
  HardDrive,
  LayoutDashboard,
  MonitorPlay,
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
      plan: 'QEMU test runner',
    },
  ],
  navGroups: [
    {
      title: 'Control Plane',
      items: [
        {
          title: 'Dashboard',
          url: '/',
          icon: LayoutDashboard,
        },
        {
          title: 'Virtual Machines',
          url: '/vms',
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
        {
          title: 'Runs',
          url: '/settings',
          icon: CirclePlay,
          badge: 'soon',
        },
      ],
    },
    {
      title: 'System',
      items: [
        {
          title: 'Console',
          url: '/vms/install-os/console',
          icon: MonitorPlay,
          badge: 'demo',
        },
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
