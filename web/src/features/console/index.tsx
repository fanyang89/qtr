import { Link } from '@tanstack/react-router'
import { ArrowLeft } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Header } from '@/components/layout/header'
import { Main } from '@/components/layout/main'
import { ThemeSwitch } from '@/components/theme-switch'
import { NoVncConsole } from './no-vnc-console'

export function ConsolePage({ name }: { name: string }) {
  return (
    <>
      <Header fixed>
        <Button variant='ghost' size='sm' asChild>
          <Link to='/vms/$name' params={{ name }}>
            <ArrowLeft className='size-4' />
            {name}
          </Link>
        </Button>
        <div className='ms-auto flex items-center gap-2'>
          <ThemeSwitch />
        </div>
      </Header>
      <Main fixed fluid className='p-0'>
        <NoVncConsole name={name} />
      </Main>
    </>
  )
}
