import { Link } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { MonitorPlay, MoreHorizontal, Play, Power, Server, Square } from 'lucide-react'
import { toast } from 'sonner'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Header } from '@/components/layout/header'
import { Main } from '@/components/layout/main'
import { Search } from '@/components/search'
import { ThemeSwitch } from '@/components/theme-switch'
import { ConfigDrawer } from '@/components/config-drawer'
import { getVms, postVmAction, type VmSummary } from '@/lib/api'

export function VmList() {
  const queryClient = useQueryClient()
  const { data: vms = [] } = useQuery({ queryKey: ['vms'], queryFn: getVms })
  const action = useMutation({
    mutationFn: ({ name, action }: { name: string; action: string }) => postVmAction(name, action),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ['vms'] })
      toast.success('VM action queued')
    },
    onError: () => toast.error('Backend API is not available'),
  })

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
        <div className='mb-6 flex flex-col gap-2 sm:flex-row sm:items-end sm:justify-between'>
          <div>
            <h1 className='text-2xl font-bold tracking-tight'>Virtual Machines</h1>
            <p className='text-muted-foreground'>Manage qtr libvirt domains.</p>
          </div>
          <Button disabled>Launch VM</Button>
        </div>
        <Card>
          <CardHeader>
            <CardTitle>Domains</CardTitle>
            <CardDescription>Live data will come from qtr web API.</CardDescription>
          </CardHeader>
          <CardContent>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Name</TableHead>
                  <TableHead>State</TableHead>
                  <TableHead>ID</TableHead>
                  <TableHead>VNC</TableHead>
                  <TableHead>Serial Log</TableHead>
                  <TableHead className='w-12' />
                </TableRow>
              </TableHeader>
              <TableBody>
                {vms.map((vm) => (
                  <VmRow key={vm.name} vm={vm} onAction={(name, vmAction) => action.mutate({ name, action: vmAction })} />
                ))}
              </TableBody>
            </Table>
          </CardContent>
        </Card>
      </Main>
    </>
  )
}

function VmRow({ vm, onAction }: { vm: VmSummary; onAction: (name: string, action: string) => void }) {
  return (
    <TableRow>
      <TableCell>
        <Link to='/vms/$name' params={{ name: vm.name }} className='flex items-center gap-2 font-medium hover:underline'>
          <Server className='size-4 text-muted-foreground' />
          {vm.name}
        </Link>
      </TableCell>
      <TableCell><Badge variant={vm.state === 'running' ? 'default' : 'secondary'}>{vm.state}</Badge></TableCell>
      <TableCell>{vm.id ?? '-'}</TableCell>
      <TableCell>{vm.vnc ? vm.vncEndpoint ?? 'enabled' : 'disabled'}</TableCell>
      <TableCell className='max-w-64 truncate'>{vm.serialLog ?? '-'}</TableCell>
      <TableCell>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant='ghost' size='icon'><MoreHorizontal className='size-4' /></Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align='end'>
            <DropdownMenuItem asChild disabled={!vm.vnc}>
              <Link to='/vms/$name/console' params={{ name: vm.name }}><MonitorPlay />Console</Link>
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => onAction(vm.name, 'start')}><Play />Start</DropdownMenuItem>
            <DropdownMenuItem onClick={() => onAction(vm.name, 'shutdown')}><Power />Shutdown</DropdownMenuItem>
            <DropdownMenuItem variant='destructive' onClick={() => onAction(vm.name, 'destroy')}><Square />Destroy</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </TableCell>
    </TableRow>
  )
}
