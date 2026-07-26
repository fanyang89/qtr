import { useEffect, useMemo, useRef, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import {
  MonitorPlay,
  MoreHorizontal,
  Play,
  Plus,
  Power,
  Search,
  Server,
  Square,
  Trash2,
} from 'lucide-react'
import { toast } from 'sonner'
import { deleteVm, getVms, postVmAction, type VmSummary } from '@/lib/api'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { ConfirmDialog } from '@/components/confirm-dialog'
import { Header } from '@/components/layout/header'
import { Main } from '@/components/layout/main'
import { PageHeading } from '@/components/page-heading'
import { StatusDot } from '@/components/status-dot'
import { ThemeSwitch } from '@/components/theme-switch'
import {
  vmMetricsByName,
  vmRuntimeMetrics,
  type VmMetricSnapshot,
} from './metrics'

const EMPTY_VMS: VmSummary[] = []

export function VmDashboard() {
  const queryClient = useQueryClient()
  const { data, isLoading } = useQuery({
    queryKey: ['vms'],
    queryFn: getVms,
    refetchInterval: 2000,
  })
  const vms = data ?? EMPTY_VMS
  const latestMetricsRef = useRef<Map<string, VmMetricSnapshot>>(new Map())
  const [previousMetrics, setPreviousMetrics] = useState<
    Map<string, VmMetricSnapshot>
  >(new Map())
  useEffect(() => {
    setPreviousMetrics(latestMetricsRef.current)
    latestMetricsRef.current = vmMetricsByName(vms)
  }, [vms])
  const [search, setSearch] = useState('')
  const [deleteTarget, setDeleteTarget] = useState<VmSummary | null>(null)

  const filteredVms = useMemo(() => {
    const term = search.toLowerCase()
    if (!term) return vms
    return vms.filter(
      (vm) =>
        vm.name.toLowerCase().includes(term) ||
        vm.state.toLowerCase().includes(term) ||
        (vm.network?.toLowerCase().includes(term) ?? false)
    )
  }, [vms, search])

  const actionMutation = useMutation({
    mutationFn: ({ name, action }: { name: string; action: string }) =>
      postVmAction(name, action),
    onSuccess: async (_, { action }) => {
      await queryClient.invalidateQueries({ queryKey: ['vms'] })
      toast.success(`VM ${action} queued`)
    },
  })

  const deleteMutation = useMutation({
    mutationFn: (name: string) => deleteVm(name),
    onSuccess: async (_, name) => {
      await queryClient.invalidateQueries({ queryKey: ['vms'] })
      toast.success(`VM ${name} deleted`)
      setDeleteTarget(null)
    },
  })

  return (
    <>
      <Header fixed>
        <div className='relative w-full max-w-xs'>
          <Search className='absolute inset-y-0 start-3 my-auto size-4 text-muted-foreground' />
          <Input
            placeholder='Search VMs...'
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className='ps-9'
          />
        </div>
        <div className='ms-auto flex items-center gap-2'>
          <ThemeSwitch />
        </div>
      </Header>
      <Main>
        <PageHeading
          eyebrow='Libvirt inventory'
          title='Virtual machines'
          description='Compute, storage, and console access for domains on this host.'
          actions={
            <Button asChild>
              <Link to='/vms/new'>
                <Plus className='size-4' />
                New VM
              </Link>
            </Button>
          }
        />

        <div className='overflow-x-auto border border-border bg-card'>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead>State</TableHead>
                <TableHead>Resources</TableHead>
                <TableHead>Utilization</TableHead>
                <TableHead>Network</TableHead>
                <TableHead>Console</TableHead>
                <TableHead className='w-12' />
              </TableRow>
            </TableHeader>
            <TableBody>
              {isLoading ? (
                <TableRow>
                  <TableCell
                    colSpan={7}
                    className='h-24 text-center text-muted-foreground'
                  >
                    Loading VMs...
                  </TableCell>
                </TableRow>
              ) : filteredVms.length === 0 ? (
                <TableRow>
                  <TableCell
                    colSpan={7}
                    className='h-24 text-center text-muted-foreground'
                  >
                    No virtual machines found.
                  </TableCell>
                </TableRow>
              ) : (
                filteredVms.map((vm) => (
                  <VmRow
                    key={vm.name}
                    vm={vm}
                    previousMetrics={previousMetrics.get(vm.name)}
                    onAction={(name, action) =>
                      actionMutation.mutate({ name, action })
                    }
                    onDelete={setDeleteTarget}
                    actionPending={
                      actionMutation.isPending &&
                      actionMutation.variables?.name === vm.name
                    }
                  />
                ))
              )}
            </TableBody>
          </Table>
        </div>
      </Main>

      <ConfirmDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => !open && setDeleteTarget(null)}
        title={`Delete ${deleteTarget?.name}?`}
        desc='This will undefine the VM from libvirt. The disk files will be kept.'
        destructive
        isLoading={deleteMutation.isPending}
        handleConfirm={() => {
          if (deleteTarget) deleteMutation.mutate(deleteTarget.name)
        }}
      />
    </>
  )
}

function VmRow({
  vm,
  previousMetrics,
  onAction,
  onDelete,
  actionPending,
}: {
  vm: VmSummary
  previousMetrics?: VmMetricSnapshot
  onAction: (name: string, action: string) => void
  onDelete: (vm: VmSummary) => void
  actionPending: boolean
}) {
  const metrics = vmRuntimeMetrics(vm, previousMetrics)
  const consoleReady = vm.vnc && vm.state === 'running'

  return (
    <TableRow>
      <TableCell>
        <Link
          to='/vms/$name'
          params={{ name: vm.name }}
          className='flex items-center gap-2 font-medium hover:underline'
        >
          <Server className='size-4 text-muted-foreground' />
          {vm.name}
        </Link>
      </TableCell>
      <TableCell>
        <StatusDot status={vm.state} />
      </TableCell>
      <TableCell>
        <span className='font-mono text-xs tabular-nums'>
          {vm.vcpus ?? '—'} CPU ·{' '}
          {vm.memoryMib ? `${Math.round(vm.memoryMib / 1024)} GiB` : '—'}
        </span>
      </TableCell>
      <TableCell>
        <span className='font-mono text-xs tabular-nums'>
          {metrics.cpu} · {metrics.memory}
        </span>
      </TableCell>
      <TableCell>{vm.network ?? '-'}</TableCell>
      <TableCell>
        {consoleReady ? (
          <Button variant='outline' size='sm' asChild>
            <Link to='/vms/$name/console' params={{ name: vm.name }}>
              <MonitorPlay className='size-4' />
              Console
            </Link>
          </Button>
        ) : (
          <Button
            variant='outline'
            size='sm'
            disabled
            title={vm.vnc ? 'VM is not running' : 'VNC is disabled'}
          >
            <MonitorPlay className='size-4' />
            Console
          </Button>
        )}
      </TableCell>
      <TableCell>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant='ghost'
              size='icon'
              aria-label={`Actions for ${vm.name}`}
              disabled={actionPending}
            >
              <MoreHorizontal className='size-4' />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align='end'>
            <DropdownMenuItem asChild disabled={!consoleReady}>
              <Link to='/vms/$name/console' params={{ name: vm.name }}>
                <MonitorPlay className='size-4' />
                Console
              </Link>
            </DropdownMenuItem>
            <DropdownMenuItem
              disabled={vm.state === 'running'}
              onClick={() => onAction(vm.name, 'start')}
            >
              <Play className='size-4' />
              Start
            </DropdownMenuItem>
            <DropdownMenuItem
              disabled={vm.state !== 'running'}
              onClick={() => onAction(vm.name, 'shutdown')}
            >
              <Power className='size-4' />
              Shutdown
            </DropdownMenuItem>
            <DropdownMenuItem
              variant='destructive'
              disabled={vm.state !== 'running'}
              onClick={() => onAction(vm.name, 'destroy')}
            >
              <Square className='size-4' />
              Destroy
            </DropdownMenuItem>
            <DropdownMenuItem
              variant='destructive'
              onClick={() => onDelete(vm)}
            >
              <Trash2 className='size-4' />
              Delete
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </TableCell>
    </TableRow>
  )
}
