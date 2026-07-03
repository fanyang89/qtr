import { Link } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { ArrowLeft, Cpu, HardDrive, MemoryStick, MonitorPlay, Network } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Header } from '@/components/layout/header'
import { Main } from '@/components/layout/main'
import { Search } from '@/components/search'
import { ThemeSwitch } from '@/components/theme-switch'
import { ConfigDrawer } from '@/components/config-drawer'
import { getVm } from '@/lib/api'

export function VmDetail({ name }: { name: string }) {
  const { data: vm } = useQuery({ queryKey: ['vms', name], queryFn: () => getVm(name) })

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
        <div className='mb-6 flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between'>
          <div>
            <Button variant='ghost' size='sm' asChild className='mb-2 -ms-2'>
              <Link to='/vms'><ArrowLeft className='size-4' />VMs</Link>
            </Button>
            <div className='flex items-center gap-3'>
              <h1 className='text-2xl font-bold tracking-tight'>{name}</h1>
              {vm && <Badge variant={vm.state === 'running' ? 'default' : 'secondary'}>{vm.state}</Badge>}
            </div>
            <p className='text-muted-foreground'>Libvirt domain details.</p>
          </div>
          <Button asChild disabled={!vm?.vnc}>
            <Link to='/vms/$name/console' params={{ name }}><MonitorPlay className='size-4' />Open Console</Link>
          </Button>
        </div>
        <div className='grid gap-4 sm:grid-cols-2 xl:grid-cols-4'>
          <InfoCard title='vCPUs' value={String(vm?.vcpus ?? '-')} icon={Cpu} />
          <InfoCard title='Memory' value={vm?.memoryMiB ? `${vm.memoryMiB} MiB` : '-'} icon={MemoryStick} />
          <InfoCard title='Network' value={vm?.network ?? '-'} icon={Network} />
          <InfoCard title='VNC' value={vm?.vncEndpoint ?? (vm?.vnc ? 'enabled' : 'disabled')} icon={MonitorPlay} />
        </div>
        <Card className='mt-6'>
          <CardHeader>
            <CardTitle className='flex items-center gap-2'><HardDrive className='size-4' />Serial Log</CardTitle>
          </CardHeader>
          <CardContent>
            <code className='rounded bg-muted px-2 py-1 text-sm'>{vm?.serialLog ?? 'Not configured'}</code>
          </CardContent>
        </Card>
      </Main>
    </>
  )
}

function InfoCard({ title, value, icon: Icon }: { title: string; value: string; icon: React.ElementType }) {
  return (
    <Card>
      <CardHeader className='flex flex-row items-center justify-between space-y-0 pb-2'>
        <CardTitle className='text-sm font-medium'>{title}</CardTitle>
        <Icon className='size-4 text-muted-foreground' />
      </CardHeader>
      <CardContent><div className='truncate text-2xl font-bold'>{value}</div></CardContent>
    </Card>
  )
}
