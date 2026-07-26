import { useState, type FormEvent } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useNavigate } from '@tanstack/react-router'
import { ArrowRight, Plus } from 'lucide-react'
import {
  createInstallJob,
  getDisks,
  getInstallJobs,
  getIsos,
  type FedoraInstallRequest,
} from '@/lib/api'
import { formatDuration, formatTimestamp } from '@/lib/format'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'
import { Header } from '@/components/layout/header'
import { Main } from '@/components/layout/main'
import { PageHeading } from '@/components/page-heading'
import { Search } from '@/components/search'
import { StatusDot } from '@/components/status-dot'
import { ThemeSwitch } from '@/components/theme-switch'

const activeStatuses = new Set(['queued', 'running'])

export function InstallationsPage({
  initialCreateOpen = false,
}: {
  initialCreateOpen?: boolean
}) {
  const [createOpen, setCreateOpen] = useState(initialCreateOpen)
  const jobs = useQuery({
    queryKey: ['install-jobs'],
    queryFn: getInstallJobs,
    refetchInterval: (query) =>
      query.state.data?.some((job) => activeStatuses.has(job.status))
        ? 2000
        : false,
  })

  return (
    <>
      <Header fixed>
        <Search placeholder='Go to...' />
        <div className='ms-auto'>
          <ThemeSwitch />
        </div>
      </Header>
      <Main>
        <PageHeading
          eyebrow='Fedora deployment'
          title='Automated installations'
          description='Unattended, persistent Fedora installation jobs executed sequentially on this host.'
          actions={
            <Button onClick={() => setCreateOpen(true)}>
              <Plus className='size-4' />
              New Automated Install
            </Button>
          }
        />

        <section className='overflow-hidden border border-border bg-card'>
          <div className='hidden grid-cols-[1fr_9rem_1fr_11rem_7rem_1.5rem] gap-4 border-b border-border px-4 py-3 font-mono text-[0.625rem] tracking-[0.14em] text-muted-foreground uppercase md:grid'>
            <span>Virtual machine</span>
            <span>Status</span>
            <span>Phase</span>
            <span>Created</span>
            <span>Duration</span>
            <span />
          </div>
          {jobs.isPending ? (
            <ListMessage>Loading automated install history…</ListMessage>
          ) : jobs.isError ? (
            <ListMessage>Automated install history is unavailable.</ListMessage>
          ) : jobs.data.length === 0 ? (
            <ListMessage>No automated install jobs yet.</ListMessage>
          ) : (
            jobs.data.map((job) => (
              <Link
                key={job.id}
                to='/installations/$id'
                params={{ id: job.id }}
                className='grid gap-3 border-b border-border px-4 py-4 transition-colors last:border-b-0 hover:bg-accent/45 md:grid-cols-[1fr_9rem_1fr_11rem_7rem_1.5rem] md:items-center md:gap-4'
              >
                <div className='min-w-0'>
                  <p className='truncate text-sm font-medium'>
                    {job.request.name}
                  </p>
                  <p className='mt-1 truncate font-mono text-[0.625rem] text-muted-foreground'>
                    {job.id}
                  </p>
                </div>
                <StatusDot status={job.status} />
                <span className='font-mono text-xs text-muted-foreground'>
                  {job.phase}
                </span>
                <span className='text-xs text-muted-foreground'>
                  {formatTimestamp(job.createdAtMs)}
                </span>
                <span className='font-mono text-xs tabular-nums'>
                  {formatDuration(job.startedAtMs, job.finishedAtMs)}
                </span>
                <ArrowRight className='hidden size-4 text-muted-foreground md:block' />
              </Link>
            ))
          )}
        </section>
      </Main>
      <InstallFedoraDialog open={createOpen} onOpenChange={setCreateOpen} />
    </>
  )
}

function InstallFedoraDialog({
  open,
  onOpenChange,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const isos = useQuery({
    queryKey: ['resources', 'isos'],
    queryFn: getIsos,
    enabled: open,
  })
  const disks = useQuery({
    queryKey: ['resources', 'disks'],
    queryFn: getDisks,
    enabled: open,
  })
  const [name, setName] = useState('')
  const [imageId, setImageId] = useState('')
  const [mediaId, setMediaId] = useState('')
  const [sshKey, setSshKey] = useState('')
  const [diskSize, setDiskSize] = useState('40GiB')
  const [memoryMib, setMemoryMib] = useState(4096)
  const [vcpus, setVcpus] = useState(2)
  const [mirror, setMirror] = useState<'official' | 'tuna'>('official')
  const [error, setError] = useState<string | null>(null)
  const create = useMutation({
    mutationFn: createInstallJob,
    onSuccess: async (job) => {
      await queryClient.invalidateQueries({ queryKey: ['install-jobs'] })
      onOpenChange(false)
      navigate({ to: '/installations/$id', params: { id: job.id } })
    },
  })

  function updateName(value: string) {
    const previousDefault = name ? `${name}.qcow2` : ''
    setName(value)
    if (!imageId || imageId === previousDefault)
      setImageId(value ? `${value}.qcow2` : '')
  }

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setError(null)
    if (!name || !mediaId || !imageId || !sshKey.trim()) {
      setError('Complete all required fields.')
      return
    }
    if (disks.data?.some((disk) => disk.id === imageId)) {
      setError('This disk ID already exists.')
      return
    }
    const request: FedoraInstallRequest = {
      name,
      mediaId,
      imageId,
      sshAuthorizedKey: sshKey.trim(),
      diskSize,
      memoryMib,
      vcpus,
      network: 'default',
      hostname: null,
      mirror,
      timeoutSecs: 7200,
      verifyTimeoutSecs: 300,
      keepFailed: false,
    }
    create.mutate(request)
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className='max-h-[90svh] overflow-y-auto sm:max-w-2xl'>
        <DialogHeader>
          <DialogTitle>Automated Fedora install</DialogTitle>
          <DialogDescription>
            Create an unattended, persistent installation job.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className='grid gap-6'>
          <div className='grid gap-4 sm:grid-cols-2'>
            <Field label='VM name' htmlFor='install-name'>
              <Input
                id='install-name'
                value={name}
                onChange={(event) => updateName(event.target.value)}
              />
            </Field>
            <Field label='Output disk ID' htmlFor='install-image'>
              <Input
                id='install-image'
                className='font-mono'
                value={imageId}
                onChange={(event) => setImageId(event.target.value)}
              />
            </Field>
          </div>
          <Field label='Installation ISO' htmlFor='install-media'>
            <Select value={mediaId} onValueChange={setMediaId}>
              <SelectTrigger id='install-media'>
                <SelectValue
                  placeholder={isos.isPending ? 'Loading ISOs…' : 'Select ISO'}
                />
              </SelectTrigger>
              <SelectContent>
                {isos.data?.map((resource) => (
                  <SelectItem key={resource.id} value={resource.id}>
                    {resource.id}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Field>
          <Field label='SSH authorized key' htmlFor='install-key'>
            <Textarea
              id='install-key'
              className='min-h-24 font-mono text-xs'
              value={sshKey}
              onChange={(event) => setSshKey(event.target.value)}
              placeholder='ssh-ed25519 AAAA…'
            />
          </Field>
          <div className='grid gap-4 sm:grid-cols-4'>
            <Field label='Disk size' htmlFor='install-disk-size'>
              <Input
                id='install-disk-size'
                value={diskSize}
                onChange={(event) => setDiskSize(event.target.value)}
              />
            </Field>
            <Field label='Memory MiB' htmlFor='install-memory'>
              <Input
                id='install-memory'
                type='number'
                min={512}
                value={memoryMib}
                onChange={(event) => setMemoryMib(Number(event.target.value))}
              />
            </Field>
            <Field label='vCPUs' htmlFor='install-vcpus'>
              <Input
                id='install-vcpus'
                type='number'
                min={1}
                value={vcpus}
                onChange={(event) => setVcpus(Number(event.target.value))}
              />
            </Field>
            <Field label='Mirror' htmlFor='install-mirror'>
              <Select
                value={mirror}
                onValueChange={(value) =>
                  setMirror(value as 'official' | 'tuna')
                }
              >
                <SelectTrigger id='install-mirror'>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value='official'>Official</SelectItem>
                  <SelectItem value='tuna'>TUNA</SelectItem>
                </SelectContent>
              </Select>
            </Field>
          </div>
          {(error || create.isError) && (
            <p className='text-sm text-destructive'>
              {error ?? 'The automated install job could not be created.'}
            </p>
          )}
          <DialogFooter>
            <Button
              type='button'
              variant='ghost'
              onClick={() => onOpenChange(false)}
            >
              Cancel
            </Button>
            <Button
              type='submit'
              disabled={create.isPending || !isos.data?.length}
            >
              {create.isPending ? 'Creating' : 'Create Job'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function Field({
  label,
  htmlFor,
  children,
}: {
  label: string
  htmlFor: string
  children: React.ReactNode
}) {
  return (
    <div className='grid gap-2'>
      <Label htmlFor={htmlFor}>{label}</Label>
      {children}
    </div>
  )
}

function ListMessage({ children }: { children: React.ReactNode }) {
  return (
    <div className='px-4 py-16 text-center text-sm text-muted-foreground'>
      {children}
    </div>
  )
}
