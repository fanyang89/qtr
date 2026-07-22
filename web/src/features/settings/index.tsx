import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { ConfigDrawer } from '@/components/config-drawer'
import { Header } from '@/components/layout/header'
import { Main } from '@/components/layout/main'
import { Search } from '@/components/search'
import { ThemeSwitch } from '@/components/theme-switch'

export function SettingsPage() {
  return (
    <>
      <Header fixed>
        <Search placeholder='Search settings' />
        <div className='ms-auto flex items-center gap-2'>
          <ThemeSwitch />
          <ConfigDrawer />
        </div>
      </Header>
      <Main>
        <div className='mb-6'>
          <h1 className='text-2xl font-bold tracking-tight'>Settings</h1>
          <p className='text-muted-foreground'>
            Host and qtr web configuration.
          </p>
        </div>
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
              <code>/api/vms/:name/vnc</code>
            </div>
          </CardContent>
        </Card>
      </Main>
    </>
  )
}
