import { useEffect, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ArrowLeft, MonitorPlay } from 'lucide-react'
import { getVm } from '@/lib/api'
import { formatBytes } from '@/lib/format'
import { Button } from '@/components/ui/button'
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
              <Section title='Storage'>
                {vm.disks?.length ? (
                  vm.disks.map((disk) => (
                    <div
                      key={`${disk.target}-${disk.path}`}
                      className='border-b border-border px-4 py-3 last:border-b-0'
                    >
                      <div className='flex items-center justify-between gap-4'>
                        <code className='font-mono text-xs'>
                          {disk.target ?? 'disk'}
                        </code>
                        <span className='text-xs text-muted-foreground'>
                          {disk.format}
                        </span>
                      </div>
                      <p className='mt-1 truncate font-mono text-[0.6875rem] text-muted-foreground'>
                        {disk.path}
                      </p>
                    </div>
                  ))
                ) : (
                  <Value label='Disks' value='None reported' />
                )}
              </Section>
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
}: {
  title: string
  children: React.ReactNode
}) {
  return (
    <section>
      <h2 className='mb-3 text-sm font-medium'>{title}</h2>
      <div className='border border-border bg-card'>{children}</div>
    </section>
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
