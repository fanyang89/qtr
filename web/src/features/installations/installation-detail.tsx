import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ArrowLeft, ExternalLink, X } from 'lucide-react'
import { cancelInstallJob, getInstallJob } from '@/lib/api'
import { formatDuration, formatTimestamp } from '@/lib/format'
import { Button } from '@/components/ui/button'
import { Header } from '@/components/layout/header'
import { Main } from '@/components/layout/main'
import { Search } from '@/components/search'
import { StatusDot } from '@/components/status-dot'
import { ThemeSwitch } from '@/components/theme-switch'

const phases = [
  'planning',
  'preflight',
  'workspace',
  'disk',
  'domain',
  'installing',
  'committing',
  'starting',
  'verifying',
  'cleanup',
]

export function InstallationDetail({ id }: { id: string }) {
  const queryClient = useQueryClient()
  const job = useQuery({
    queryKey: ['install-jobs', id],
    queryFn: () => getInstallJob(id),
    refetchInterval: (query) =>
      query.state.data &&
      ['queued', 'running'].includes(query.state.data.status)
        ? 2000
        : false,
  })
  const cancel = useMutation({
    mutationFn: () => cancelInstallJob(id),
    onSuccess: (updated) => {
      queryClient.setQueryData(['install-jobs', id], updated)
      queryClient.invalidateQueries({ queryKey: ['install-jobs'] })
    },
  })

  if (job.isPending) return <DetailMessage>Loading installation…</DetailMessage>
  if (job.isError)
    return <DetailMessage>Installation job was not found.</DetailMessage>

  const value = job.data
  const active = ['queued', 'running'].includes(value.status)
  const currentPhase = phases.indexOf(value.phase)

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
          <Link to='/installations'>
            <ArrowLeft className='size-4' />
            Installations
          </Link>
        </Button>
        <div className='mb-9 flex flex-col gap-5 border-b border-border pb-7 sm:flex-row sm:items-end sm:justify-between'>
          <div>
            <p className='mb-3 font-mono text-[0.625rem] tracking-[0.17em] text-muted-foreground uppercase'>
              Fedora installation
            </p>
            <div className='flex flex-wrap items-center gap-4'>
              <h1 className='text-3xl font-medium tracking-[-0.04em] sm:text-4xl'>
                {value.request.name}
              </h1>
              <StatusDot status={value.status} />
            </div>
            <p className='mt-2 font-mono text-[0.6875rem] text-muted-foreground'>
              {value.id}
            </p>
          </div>
          <div className='flex gap-2'>
            {value.status === 'succeeded' && (
              <Button variant='outline' asChild>
                <Link to='/vms/$name' params={{ name: value.request.name }}>
                  Open VM <ExternalLink className='size-4' />
                </Link>
              </Button>
            )}
            {active && (
              <Button
                variant='outline'
                onClick={() => cancel.mutate()}
                disabled={cancel.isPending}
              >
                <X className='size-4' />
                {value.cancelRequested ? 'Cancel requested' : 'Cancel'}
              </Button>
            )}
          </div>
        </div>

        <div className='grid gap-8 lg:grid-cols-[1.3fr_1fr]'>
          <section>
            <h2 className='mb-4 text-sm font-medium'>Execution</h2>
            <div className='border border-border bg-card'>
              {phases.map((phase, index) => {
                const complete =
                  value.status === 'succeeded' || index < currentPhase
                const current = phase === value.phase && active
                return (
                  <div
                    key={phase}
                    className='grid grid-cols-[1.5rem_1fr_auto] items-center gap-3 border-b border-border px-4 py-3 last:border-b-0'
                  >
                    <span
                      className={`size-2 rounded-full ${complete ? 'bg-status-success' : current ? 'bg-primary' : 'border border-border'}`}
                    />
                    <span
                      className={`text-sm capitalize ${!complete && !current ? 'text-muted-foreground' : ''}`}
                    >
                      {phase}
                    </span>
                    {current && (
                      <span className='font-mono text-[0.625rem] text-muted-foreground uppercase'>
                        active
                      </span>
                    )}
                  </div>
                )
              })}
            </div>
          </section>

          <section>
            <h2 className='mb-4 text-sm font-medium'>Request</h2>
            <dl className='border border-border bg-card'>
              <Value label='Media' value={value.request.mediaId} mono />
              <Value label='Image' value={value.request.imageId} mono />
              <Value label='Disk size' value={value.request.diskSize} />
              <Value label='Memory' value={`${value.request.memoryMib} MiB`} />
              <Value label='vCPUs' value={String(value.request.vcpus)} />
              <Value label='Mirror' value={value.request.mirror} />
              <Value
                label='Created'
                value={formatTimestamp(value.createdAtMs)}
              />
              <Value
                label='Started'
                value={formatTimestamp(value.startedAtMs)}
              />
              <Value
                label='Duration'
                value={formatDuration(value.startedAtMs, value.finishedAtMs)}
              />
            </dl>
          </section>
        </div>

        {value.error && (
          <section className='mt-8 border border-destructive/40 bg-destructive/5 p-5'>
            <h2 className='mb-3 text-sm font-medium text-destructive'>
              Installation error
            </h2>
            <pre className='overflow-x-auto font-mono text-xs leading-5 whitespace-pre-wrap'>
              {value.error}
            </pre>
          </section>
        )}
      </Main>
    </>
  )
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
      <dt className='text-xs text-muted-foreground'>{label}</dt>
      <dd className={mono ? 'truncate font-mono text-xs' : 'text-sm'}>
        {value}
      </dd>
    </div>
  )
}

function DetailMessage({ children }: { children: React.ReactNode }) {
  return (
    <div className='grid min-h-svh place-items-center text-sm text-muted-foreground'>
      {children}
    </div>
  )
}
