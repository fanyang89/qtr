import { useEffect, useMemo, useRef, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import {
  MonitorPlay,
  MoreHorizontal,
  Pencil,
  Play,
  Plus,
  Power,
  Search,
  Server,
  Square,
  Trash2,
} from 'lucide-react'
import { toast } from 'sonner'
import {
  createVm,
  deleteVm,
  getVms,
  postVmAction,
  updateVm,
  type VmCreateInput,
  type VmSummary,
  type VmUpdateInput,
} from '@/lib/api'
import { Badge } from '@/components/ui/badge'
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
import { ThemeSwitch } from '@/components/theme-switch'
import {
  vmMetricsByName,
  vmRuntimeMetrics,
  type VmMetricSnapshot,
} from './metrics'
import { VmFormDialog } from './vm-form-dialog'

export function VmDashboard() {
  const queryClient = useQueryClient()
  const { data: vms = [], isLoading } = useQuery({
    queryKey: ['vms'],
    queryFn: getVms,
    refetchInterval: 2000,
  })
  const latestMetricsRef = useRef<Map<string, VmMetricSnapshot>>(new Map())
  const [previousMetrics, setPreviousMetrics] = useState<
    Map<string, VmMetricSnapshot>
  >(new Map())
  useEffect(() => {
    setPreviousMetrics(latestMetricsRef.current)
    latestMetricsRef.current = vmMetricsByName(vms)
  }, [vms])
  const [search, setSearch] = useState('')
  const [formMode, setFormMode] = useState<'create' | 'edit' | null>(null)
  const [selectedVm, setSelectedVm] = useState<VmSummary | null>(null)
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

  const createMutation = useMutation({
    mutationFn: createVm,
    onSuccess: async (vm) => {
      await queryClient.invalidateQueries({ queryKey: ['vms'] })
      toast.success(`VM ${vm.name} created`)
      setFormMode(null)
    },
  })

  const updateMutation = useMutation({
    mutationFn: ({ name, input }: { name: string; input: VmUpdateInput }) =>
      updateVm(name, input),
    onSuccess: async (vm) => {
      await queryClient.invalidateQueries({ queryKey: ['vms'] })
      await queryClient.invalidateQueries({ queryKey: ['vms', vm.name] })
      toast.success(`VM ${vm.name} updated`)
      setFormMode(null)
      setSelectedVm(null)
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

  async function handleFormSubmit(input: VmCreateInput | VmUpdateInput) {
    if (formMode === 'create') {
      await createMutation.mutateAsync(input as VmCreateInput)
      return
    }
    if (formMode === 'edit' && selectedVm) {
      await updateMutation.mutateAsync({
        name: selectedVm.name,
        input: input as VmUpdateInput,
      })
      return
    }
    throw new Error('Invalid form mode')
  }

  function openCreate() {
    setSelectedVm(null)
    setFormMode('create')
  }

  function openEdit(vm: VmSummary) {
    setSelectedVm(vm)
    setFormMode('edit')
  }

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
        <div className='mb-6 flex flex-col gap-2 sm:flex-row sm:items-end sm:justify-between'>
          <div>
            <h1 className='text-2xl font-bold tracking-tight'>
              Virtual Machines
            </h1>
            <p className='text-muted-foreground'>Manage qtr libvirt domains.</p>
          </div>
          <Button onClick={openCreate}>
            <Plus className='size-4' />
            Create VM
          </Button>
        </div>

        <div className='rounded-lg border bg-card'>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead>State</TableHead>
                <TableHead>vCPUs</TableHead>
                <TableHead>Memory</TableHead>
                <TableHead>CPU</TableHead>
                <TableHead>Mem %</TableHead>
                <TableHead>TX</TableHead>
                <TableHead>RX</TableHead>
                <TableHead>Network</TableHead>
                <TableHead>VNC</TableHead>
                <TableHead className='w-12' />
              </TableRow>
            </TableHeader>
            <TableBody>
              {isLoading ? (
                <TableRow>
                  <TableCell
                    colSpan={11}
                    className='h-24 text-center text-muted-foreground'
                  >
                    Loading VMs...
                  </TableCell>
                </TableRow>
              ) : filteredVms.length === 0 ? (
                <TableRow>
                  <TableCell
                    colSpan={11}
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
                    onEdit={openEdit}
                    onDelete={setDeleteTarget}
                  />
                ))
              )}
            </TableBody>
          </Table>
        </div>
      </Main>

      <VmFormDialog
        open={formMode !== null}
        onOpenChange={(open) => {
          if (!open) {
            setFormMode(null)
            setSelectedVm(null)
          }
        }}
        mode={formMode ?? 'create'}
        vm={selectedVm}
        onSubmit={handleFormSubmit}
        isLoading={createMutation.isPending || updateMutation.isPending}
      />

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
  onEdit,
  onDelete,
}: {
  vm: VmSummary
  previousMetrics?: VmMetricSnapshot
  onAction: (name: string, action: string) => void
  onEdit: (vm: VmSummary) => void
  onDelete: (vm: VmSummary) => void
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
        <Badge variant={vm.state === 'running' ? 'default' : 'secondary'}>
          {vm.state}
        </Badge>
      </TableCell>
      <TableCell>{vm.vcpus ?? '-'}</TableCell>
      <TableCell>
        {vm.memoryMiB ? `${Math.round(vm.memoryMiB / 1024)} GiB` : '-'}
      </TableCell>
      <TableCell>{metrics.cpu}</TableCell>
      <TableCell>{metrics.memory}</TableCell>
      <TableCell>{metrics.tx}</TableCell>
      <TableCell>{metrics.rx}</TableCell>
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
            <Button variant='ghost' size='icon'>
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
            <DropdownMenuItem onClick={() => onAction(vm.name, 'start')}>
              <Play className='size-4' />
              Start
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => onAction(vm.name, 'shutdown')}>
              <Power className='size-4' />
              Shutdown
            </DropdownMenuItem>
            <DropdownMenuItem
              variant='destructive'
              onClick={() => onAction(vm.name, 'destroy')}
            >
              <Square className='size-4' />
              Destroy
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => onEdit(vm)}>
              <Pencil className='size-4' />
              Edit
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
