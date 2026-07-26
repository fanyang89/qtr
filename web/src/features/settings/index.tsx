import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { getApiToken, getHealth, setApiToken } from '@/lib/api'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Header } from '@/components/layout/header'
import { Main } from '@/components/layout/main'
import { PageHeading } from '@/components/page-heading'
import { Search } from '@/components/search'
import { StatusDot } from '@/components/status-dot'
import { ThemeSwitch } from '@/components/theme-switch'

export function SettingsPage() {
  const queryClient = useQueryClient()
  const [token, setToken] = useState(getApiToken)
  const health = useQuery({
    queryKey: ['health'],
    queryFn: getHealth,
    refetchInterval: 30_000,
  })

  function saveToken() {
    setApiToken(token.trim())
    queryClient.invalidateQueries()
    toast.success('API token updated')
  }

  function clearToken() {
    setApiToken('')
    queryClient.clear()
    window.location.assign('/access')
  }

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
          eyebrow='Local configuration'
          title='Settings'
          description='Session credentials and backend connectivity for this browser.'
        />
        <div className='grid gap-6 lg:grid-cols-2'>
          <Card>
            <CardHeader>
              <CardTitle>API access</CardTitle>
              <CardDescription>
                Stored only in this browser tab.
              </CardDescription>
            </CardHeader>
            <CardContent className='grid gap-4'>
              <div className='grid gap-2'>
                <Label htmlFor='api-token'>Bearer token</Label>
                <Input
                  id='api-token'
                  type='password'
                  autoComplete='off'
                  className='font-mono'
                  value={token}
                  onChange={(event) => setToken(event.target.value)}
                />
              </div>
              <div className='flex gap-2'>
                <Button onClick={saveToken}>Save Token</Button>
                <Button variant='ghost' onClick={clearToken}>
                  Clear Session
                </Button>
              </div>
            </CardContent>
          </Card>
          <Card>
            <CardHeader>
              <CardTitle className='flex items-center justify-between'>
                Backend
                <StatusDot
                  status={health.data?.ok ? 'running' : 'interrupted'}
                />
              </CardTitle>
              <CardDescription>Live server health information.</CardDescription>
            </CardHeader>
            <CardContent>
              <dl className='border border-border'>
                <Value
                  label='Status'
                  value={
                    health.isPending
                      ? 'Checking'
                      : health.data?.ok
                        ? 'Available'
                        : 'Degraded'
                  }
                />
                <Value
                  label='Version'
                  value={health.data?.version ?? '—'}
                  mono
                />
                <Value
                  label='Libvirt URI'
                  value={health.data?.libvirtUri ?? '—'}
                  mono
                />
                <Value label='API base' value='/api/v1' mono />
              </dl>
            </CardContent>
          </Card>
        </div>
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
      <dd className={mono ? 'font-mono text-xs' : 'text-sm'}>{value}</dd>
    </div>
  )
}
