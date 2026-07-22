import { useQuery } from '@tanstack/react-query'
import { Activity, MonitorPlay, Server, TerminalSquare } from 'lucide-react'
import { getHealth, getVms } from '@/lib/api'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { ConfigDrawer } from '@/components/config-drawer'
import { Header } from '@/components/layout/header'
import { Main } from '@/components/layout/main'
import { Search } from '@/components/search'
import { ThemeSwitch } from '@/components/theme-switch'

export function Dashboard() {
  const { data: health } = useQuery({
    queryKey: ['health'],
    queryFn: getHealth,
  })
  const { data: vms = [] } = useQuery({ queryKey: ['vms'], queryFn: getVms })
  const running = vms.filter((vm) => vm.state === 'running').length
  const withVnc = vms.filter((vm) => vm.vnc).length

  return (
    <>
      <Header fixed>
        <Search placeholder='Search VMs' />
        <div className='ms-auto flex items-center gap-2'>
          <ThemeSwitch />
          <ConfigDrawer />
        </div>
      </Header>
      <Main>
        <div className='mb-6 flex flex-col gap-2'>
          <h1 className='text-2xl font-bold tracking-tight'>qtr Dashboard</h1>
          <p className='text-muted-foreground'>
            QEMU/libvirt VM control plane.
          </p>
        </div>
        <div className='grid gap-4 sm:grid-cols-2 xl:grid-cols-4'>
          <MetricCard
            title='Libvirt'
            value={health?.libvirtUri ?? 'qemu:///system'}
            icon={TerminalSquare}
            badge={health?.ok ? 'online' : 'planned'}
          />
          <MetricCard
            title='Virtual Machines'
            value={String(vms.length)}
            icon={Server}
          />
          <MetricCard title='Running' value={String(running)} icon={Activity} />
          <MetricCard
            title='VNC Ready'
            value={String(withVnc)}
            icon={MonitorPlay}
          />
        </div>
        <Card className='mt-6'>
          <CardHeader>
            <CardTitle>Recent VMs</CardTitle>
          </CardHeader>
          <CardContent className='grid gap-3'>
            {vms.map((vm) => (
              <div
                key={vm.name}
                className='flex items-center justify-between rounded-lg border p-3'
              >
                <div>
                  <div className='font-medium'>{vm.name}</div>
                  <div className='text-sm text-muted-foreground'>
                    {vm.serialLog ?? 'No serial log'}
                  </div>
                </div>
                <Badge
                  variant={vm.state === 'running' ? 'default' : 'secondary'}
                >
                  {vm.state}
                </Badge>
              </div>
            ))}
          </CardContent>
        </Card>
      </Main>
    </>
  )
}

function MetricCard({
  title,
  value,
  icon: Icon,
  badge,
}: {
  title: string
  value: string
  icon: React.ElementType
  badge?: string
}) {
  return (
    <Card>
      <CardHeader className='flex flex-row items-center justify-between space-y-0 pb-2'>
        <CardTitle className='text-sm font-medium'>{title}</CardTitle>
        <Icon className='size-4 text-muted-foreground' />
      </CardHeader>
      <CardContent>
        <div className='truncate text-2xl font-bold'>{value}</div>
        {badge && <Badge className='mt-2'>{badge}</Badge>}
      </CardContent>
    </Card>
  )
}
