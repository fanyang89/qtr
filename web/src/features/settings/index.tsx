import { useState } from 'react'
import { getApiToken, setApiToken } from '@/lib/api'
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
import { Search } from '@/components/search'
import { ThemeSwitch } from '@/components/theme-switch'

export function SettingsPage() {
  const [token, setToken] = useState(getApiToken)

  function saveToken() {
    setApiToken(token.trim())
    window.location.assign('/')
  }

  return (
    <>
      <Header fixed>
        <Search placeholder='Go to...' />
        <div className='ms-auto flex items-center gap-2'>
          <ThemeSwitch />
        </div>
      </Header>
      <Main>
        <div className='mb-6'>
          <h1 className='text-2xl font-bold tracking-tight'>Settings</h1>
          <p className='text-muted-foreground'>
            Host and qtr web configuration.
          </p>
        </div>
        <div className='grid gap-6 lg:grid-cols-2'>
          <Card>
            <CardHeader>
              <CardTitle>API Access</CardTitle>
              <CardDescription>
                Bearer token for this browser tab.
              </CardDescription>
            </CardHeader>
            <CardContent className='grid gap-4'>
              <div className='grid gap-2'>
                <Label htmlFor='api-token'>Bearer token</Label>
                <Input
                  id='api-token'
                  type='password'
                  autoComplete='off'
                  value={token}
                  onChange={(event) => setToken(event.target.value)}
                />
              </div>
              <Button className='w-fit' onClick={saveToken}>
                Save Token
              </Button>
            </CardContent>
          </Card>
          <Card>
            <CardHeader>
              <CardTitle>Backend</CardTitle>
              <CardDescription>Expected qtr web defaults.</CardDescription>
            </CardHeader>
            <CardContent className='grid gap-3 text-sm'>
              <div className='flex justify-between gap-4'>
                <span className='text-muted-foreground'>Listen</span>
                <code>127.0.0.1:8080</code>
              </div>
              <div className='flex justify-between gap-4'>
                <span className='text-muted-foreground'>Libvirt URI</span>
                <code>qemu:///system</code>
              </div>
              <div className='flex justify-between gap-4'>
                <span className='text-muted-foreground'>VNC WebSocket</span>
                <code>/api/v1/vms/:name/vnc</code>
              </div>
            </CardContent>
          </Card>
        </div>
      </Main>
    </>
  )
}
