import { useEffect, useState } from 'react'
import axios from 'axios'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ArrowLeft, MonitorPlay, Plus, Unplug } from 'lucide-react'
import { toast } from 'sonner'
import {
  attachDisk,
  detachDisk,
  getDisks,
  getVm,
  type ManagedImage,
  type VmDisk,
} from '@/lib/api'
import { formatBytes } from '@/lib/format'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { ConfirmDialog } from '@/components/confirm-dialog'
import { Header } from '@/components/layout/header'
import { Main } from '@/components/layout/main'
import { Search } from '@/components/search'
import { StatusDot } from '@/components/status-dot'
import { ThemeSwitch } from '@/components/theme-switch'
import { vmRuntimeMetrics, type VmMetricSnapshot } from './metrics'

export function VmDetail({ name }: { name: string }) {
  const [previousMetrics, setPreviousMetrics] = useState<VmMetricSnapshot>()
  const {
    data: vm,
    isPending,
    isError,
  } = useQuery({
    queryKey: ['vms', name],
    queryFn: () => getVm(name),
    refetchInterval: 2000,
  })
  const metrics = vmRuntimeMetrics(vm, previousMetrics)

  useEffect(() => {
    if (vm?.metrics) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setPreviousMetrics(vm.metrics)
    }
  }, [vm?.metrics])

  return (
    <>
      <Header fixed>
        <Search placeholder='Go to...' />
        <div className='ms-auto'>
          <ThemeSwitch />
        </div>
      </Header>
      <Main>
        <Button variant='ghost' size='sm' asChild className='-ms-2 mb-5'>
          <Link to='/vms'>
            <ArrowLeft className='size-4' />
            Virtual machines
          </Link>
        </Button>
        {isPending ? (
          <Message>Loading virtual machine…</Message>
        ) : isError || !vm ? (
          <Message>Virtual machine was not found.</Message>
        ) : (
          <>
            <div className='mb-9 flex flex-col gap-5 border-b border-border pb-7 sm:flex-row sm:items-end sm:justify-between'>
              <div>
                <p className='mb-3 font-mono text-[0.625rem] tracking-[0.17em] text-muted-foreground uppercase'>
                  Libvirt domain
                </p>
                <div className='flex items-center gap-4'>
                  <h1 className='text-3xl font-medium tracking-[-0.04em] sm:text-4xl'>
                    {name}
                  </h1>
                  <StatusDot status={vm.state} />
                </div>
                <p className='mt-2 font-mono text-[0.6875rem] text-muted-foreground'>
                  {vm.id ?? 'inactive domain'}
                </p>
              </div>
              {vm.vnc && vm.state === 'running' && (
                <Button asChild>
                  <Link to='/vms/$name/console' params={{ name }}>
                    <MonitorPlay className='size-4' />
                    Open Console
                  </Link>
                </Button>
              )}
            </div>

            <div className='grid gap-8 lg:grid-cols-2'>
              <Section title='Compute'>
                <Value label='vCPUs' value={String(vm.vcpus ?? '—')} />
                <Value
                  label='Memory'
                  value={vm.memoryMib ? `${vm.memoryMib} MiB` : '—'}
                />
                <Value label='CPU usage' value={metrics.cpu} mono />
                <Value label='Memory used' value={metrics.memory} mono />
              </Section>
              <Section title='Connectivity'>
                <Value label='Network' value={vm.network ?? '—'} />
                <Value label='TX rate' value={metrics.tx} mono />
                <Value label='RX rate' value={metrics.rx} mono />
                <Value
                  label='VNC'
                  value={vm.vncEndpoint ?? (vm.vnc ? 'enabled' : 'disabled')}
                  mono
                />
              </Section>
              <VmStorage name={name} state={vm.state} disks={vm.disks ?? []} />
              <Section title='Configuration'>
                <Value label='Boot' value={vm.boot?.join(' → ') ?? '—'} />
                <Value label='CD-ROM' value={vm.cdrom ?? 'Not attached'} mono />
                <Value label='Graphics' value={vm.graphics ?? '—'} />
                <Value
                  label='Serial log'
                  value={vm.serialLog ?? 'Not configured'}
                  mono
                />
              </Section>
            </div>

            {vm.metrics && (
              <p className='mt-8 font-mono text-[0.625rem] text-muted-foreground uppercase'>
                Sample {formatBytes(vm.metrics.txBytes + vm.metrics.rxBytes)}{' '}
                network I/O total
              </p>
            )}
          </>
        )}
      </Main>
    </>
  )
}

function Section({
  title,
  children,
  action,
}: {
  title: string
  children: React.ReactNode
  action?: React.ReactNode
}) {
  return (
    <section>
      <div className='mb-3 flex min-h-8 items-center justify-between gap-3'>
        <h2 className='text-sm font-medium'>{title}</h2>
        {action}
      </div>
      <div className='border border-border bg-card'>{children}</div>
    </section>
  )
}

export function VmStorage({
  name,
  state,
  disks,
}: {
  name: string
  state: string
  disks: VmDisk[]
}) {
  const queryClient = useQueryClient()
  const [open, setOpen] = useState(false)
  const [imageId, setImageId] = useState('')
  const [bus, setBus] = useState<'virtio-blk' | 'virtio-scsi'>('virtio-blk')
  const [detachImage, setDetachImage] = useState<ManagedImage | null>(null)
  const [error, setError] = useState<string | null>(null)
  const images = useQuery({
    queryKey: ['resources', 'disks'],
    queryFn: getDisks,
  })
  const refresh = async () => {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ['vms', name] }),
      queryClient.invalidateQueries({ queryKey: ['vms'] }),
      queryClient.invalidateQueries({ queryKey: ['resources', 'disks'] }),
    ])
  }
  const attach = useMutation({
    mutationFn: () => attachDisk(name, imageId, bus),
    onSuccess: async () => {
      await refresh()
      toast.success(`${imageId} attached`)
      setOpen(false)
      setImageId('')
    },
    onError: (attachError) =>
      setError(errorDetail(attachError, 'Attach failed.')),
  })
  const detach = useMutation({
    mutationFn: (image: ManagedImage) => detachDisk(name, image.id),
    onSuccess: async (_, image) => {
      await refresh()
      toast.success(`${image.id} detached`)
      setDetachImage(null)
    },
    onError: (detachError) =>
      setError(errorDetail(detachError, 'Detach failed.')),
  })
  const inactive = state === 'shutoff'
  const available = (images.data ?? []).filter(
    (image) =>
      image.status === 'ready' &&
      image.attachments.length === 0 &&
      !image.reservedByJobId
  )

  return (
    <Section
      title='Storage'
      action={
        <Button
          size='sm'
          variant='outline'
          disabled={!inactive || available.length === 0}
          title={inactive ? 'Attach managed disk' : 'Shut down the VM first'}
          onClick={() => {
            setError(null)
            setOpen(true)
          }}
        >
          <Plus className='size-4' /> Attach Disk
        </Button>
      }
    >
      {disks.length ? (
        disks.map((disk) => {
          const image = images.data?.find((candidate) =>
            candidate.attachments.some(
              (attachment) =>
                attachment.vmName === name && attachment.target === disk.target
            )
          )
          return (
            <div
              key={`${disk.target}-${disk.path}`}
              className='flex items-center gap-3 border-b border-border px-4 py-3 last:border-b-0'
            >
              <div className='min-w-0 flex-1'>
                <div className='flex items-center gap-3'>
                  <code className='font-mono text-xs'>
                    {disk.target ?? 'disk'}
                  </code>
                  <span className='text-xs text-muted-foreground'>
                    {disk.format}
                  </span>
                </div>
                <p className='mt-1 truncate font-mono text-[0.6875rem] text-muted-foreground'>
                  {image?.id ?? disk.path}
                </p>
              </div>
              {image && (
                <Button
                  variant='ghost'
                  size='icon'
                  aria-label={`Detach ${image.id}`}
                  disabled={!inactive || disks.length <= 1}
                  title={inactive ? 'Detach disk' : 'Shut down the VM first'}
                  onClick={() => {
                    setError(null)
                    setDetachImage(image)
                  }}
                >
                  <Unplug className='size-4 text-muted-foreground' />
                </Button>
              )}
            </div>
          )
        })
      ) : (
        <Value label='Disks' value='None reported' />
      )}
      {error && (
        <p className='border-t border-border px-4 py-3 text-sm text-destructive'>
          {error}
        </p>
      )}
      <Dialog
        open={open}
        onOpenChange={(next) => !attach.isPending && setOpen(next)}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Attach managed disk</DialogTitle>
            <DialogDescription>
              Add an available disk to this powered-off VM.
            </DialogDescription>
          </DialogHeader>
          <div className='grid gap-4'>
            <div className='grid gap-2'>
              <Label>Managed disk</Label>
              <Select value={imageId} onValueChange={setImageId}>
                <SelectTrigger className='w-full'>
                  <SelectValue placeholder='Select disk' />
                </SelectTrigger>
                <SelectContent>
                  {available.map((image) => (
                    <SelectItem key={image.id} value={image.id}>
                      {image.id} ·{' '}
                      {formatBytes(image.virtualSizeBytes ?? image.sizeBytes)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className='grid gap-2'>
              <Label>Bus</Label>
              <Select
                value={bus}
                onValueChange={(value) => setBus(value as typeof bus)}
              >
                <SelectTrigger className='w-full'>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value='virtio-blk'>VirtIO block</SelectItem>
                  <SelectItem value='virtio-scsi'>VirtIO SCSI</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>
          {error && <p className='text-sm text-destructive'>{error}</p>}
          <DialogFooter>
            <Button
              variant='ghost'
              disabled={attach.isPending}
              onClick={() => setOpen(false)}
            >
              Cancel
            </Button>
            <Button
              disabled={!imageId || attach.isPending}
              onClick={() => attach.mutate()}
            >
              {attach.isPending ? 'Attaching…' : 'Attach Disk'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <ConfirmDialog
        open={detachImage != null}
        onOpenChange={(next) => !next && setDetachImage(null)}
        title={`Detach ${detachImage?.id ?? 'disk'}?`}
        desc='The disk remains in managed storage.'
        confirmText='Detach'
        isLoading={detach.isPending}
        handleConfirm={() => detachImage && detach.mutate(detachImage)}
      >
        {error && <p className='text-sm text-destructive'>{error}</p>}
      </ConfirmDialog>
    </Section>
  )
}

function errorDetail(error: unknown, fallback: string): string {
  const detail = axios.isAxiosError(error) ? error.response?.data?.detail : null
  return typeof detail === 'string' ? detail : fallback
}

function Value({
  label,
  value,
  mono = false,
}: {
  label: string
  value: string
  mono?: boolean
}) {
  return (
    <div className='grid grid-cols-[7rem_1fr] gap-4 border-b border-border px-4 py-3 last:border-b-0'>
      <span className='text-xs text-muted-foreground'>{label}</span>
      <span className={`truncate text-sm ${mono ? 'font-mono text-xs' : ''}`}>
        {value}
      </span>
    </div>
  )
}

function Message({ children }: { children: React.ReactNode }) {
  return (
    <div className='py-24 text-center text-sm text-muted-foreground'>
      {children}
    </div>
  )
}
