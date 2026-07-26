import { useDeferredValue, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { Disc3, HardDrive, Search as SearchIcon } from 'lucide-react'
import { getDisks, getIsos } from '@/lib/api'
import { formatBytes, formatTimestamp } from '@/lib/format'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Header } from '@/components/layout/header'
import { Main } from '@/components/layout/main'
import { PageHeading } from '@/components/page-heading'
import { Search } from '@/components/search'
import { ThemeSwitch } from '@/components/theme-switch'
import { DeleteIsoButton, IsoUploadButton } from './iso-actions'

export function ResourceInventory({ kind }: { kind: 'disks' | 'isos' }) {
  const [search, setSearch] = useState('')
  const deferredSearch = useDeferredValue(search.trim().toLowerCase())
  const query = useQuery({
    queryKey: ['resources', kind],
    queryFn: kind === 'disks' ? getDisks : getIsos,
  })
  const resources = (query.data ?? []).filter((resource) =>
    resource.id.toLowerCase().includes(deferredSearch)
  )
  const Icon = kind === 'disks' ? HardDrive : Disc3
  const title = kind === 'disks' ? 'Disks' : 'ISOs'
  const singular = kind === 'disks' ? 'Disk' : 'ISO'

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
          eyebrow='Managed resources'
          title={title}
          description={
            kind === 'disks'
              ? 'Managed virtual disks available to machines on this host.'
              : 'Read-only ISO installation media available to deployment jobs.'
          }
          actions={kind === 'isos' ? <IsoUploadButton /> : undefined}
        />

        <div className='mb-5 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between'>
          <div className='flex gap-1 border border-border bg-card p-1'>
            <Button
              variant={kind === 'disks' ? 'secondary' : 'ghost'}
              size='sm'
              asChild
            >
              <Link to='/resources/disks'>Disks</Link>
            </Button>
            <Button
              variant={kind === 'isos' ? 'secondary' : 'ghost'}
              size='sm'
              asChild
            >
              <Link to='/resources/isos'>ISOs</Link>
            </Button>
          </div>
          <div className='relative w-full sm:w-72'>
            <SearchIcon className='absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground' />
            <Input
              value={search}
              onChange={(event) => setSearch(event.target.value)}
              placeholder={`Filter ${title}`}
              className='ps-9'
            />
          </div>
        </div>

        <section className='overflow-hidden border border-border bg-card'>
          <div className='grid grid-cols-[1fr_auto_2.25rem] gap-4 border-b border-border px-4 py-3 font-mono text-[0.625rem] tracking-[0.14em] text-muted-foreground uppercase sm:grid-cols-[1fr_8rem_13rem_2.25rem]'>
            <span>{singular} ID</span>
            <span>Size</span>
            <span className='hidden sm:block'>Modified</span>
            <span />
          </div>
          {query.isPending ? (
            <ResourceMessage>Reading resource root…</ResourceMessage>
          ) : query.isError ? (
            <ResourceMessage>Resource root is unavailable.</ResourceMessage>
          ) : resources.length === 0 ? (
            <ResourceMessage>
              {search
                ? 'No resources match this filter.'
                : `No ${title} found.`}
            </ResourceMessage>
          ) : (
            resources.map((resource) => (
              <div
                key={resource.id}
                className='grid grid-cols-[1fr_auto_2.25rem] items-center gap-4 border-b border-border px-4 py-4 last:border-b-0 sm:grid-cols-[1fr_8rem_13rem_2.25rem]'
              >
                <span className='flex min-w-0 items-center gap-3'>
                  <Icon className='size-4 shrink-0 text-muted-foreground' />
                  <code className='truncate font-mono text-xs'>
                    {resource.id}
                  </code>
                </span>
                <span className='font-mono text-xs tabular-nums'>
                  {formatBytes(resource.sizeBytes)}
                </span>
                <span className='hidden text-sm text-muted-foreground sm:block'>
                  {formatTimestamp(resource.modifiedAtMs)}
                </span>
                <span>
                  {kind === 'isos' && <DeleteIsoButton iso={resource} />}
                </span>
              </div>
            ))
          )}
        </section>
      </Main>
    </>
  )
}

function ResourceMessage({ children }: { children: React.ReactNode }) {
  return (
    <div className='px-4 py-16 text-center text-sm text-muted-foreground'>
      {children}
    </div>
  )
}
