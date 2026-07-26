import { useState, type FormEvent } from 'react'
import axios from 'axios'
import { useQuery } from '@tanstack/react-query'
import { ArrowRight, CircleAlert } from 'lucide-react'
import { getHealth, setApiToken, validateSession } from '@/lib/api'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { ThemeSwitch } from '@/components/theme-switch'

export function AccessPage() {
  const [token, setToken] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)
  const health = useQuery({
    queryKey: ['health'],
    queryFn: getHealth,
    retry: false,
  })

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const value = token.trim()
    if (!value) {
      setError('Enter an API token.')
      return
    }

    setSubmitting(true)
    setError(null)
    setApiToken(value)
    try {
      await validateSession()
      window.location.assign('/vms')
    } catch (requestError) {
      setApiToken('')
      setError(
        axios.isAxiosError(requestError) &&
          requestError.response?.status === 401
          ? 'The token was not accepted.'
          : 'The server could not verify this token.'
      )
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <main className='relative min-h-svh overflow-hidden bg-background px-6 py-8 text-foreground sm:px-10'>
      <div className='absolute inset-x-0 top-0 h-px bg-border' />
      <div className='mx-auto flex min-h-[calc(100svh-4rem)] max-w-6xl flex-col'>
        <header className='flex items-center justify-between border-b border-border pb-5'>
          <div className='flex items-baseline gap-3'>
            <span className='font-mono text-sm font-semibold tracking-[0.18em]'>
              QTR
            </span>
            <span className='text-xs text-muted-foreground'>host control</span>
          </div>
          <ThemeSwitch />
        </header>

        <div className='grid flex-1 items-center gap-14 py-16 lg:grid-cols-[1fr_26rem]'>
          <section className='max-w-xl'>
            <p className='mb-5 font-mono text-[0.6875rem] tracking-[0.18em] text-muted-foreground uppercase'>
              Local virtual infrastructure
            </p>
            <h1 className='text-5xl leading-[0.96] font-medium tracking-[-0.055em] sm:text-7xl'>
              Quiet control,
              <br />
              close to metal.
            </h1>
            <p className='mt-7 max-w-md text-sm leading-6 text-muted-foreground'>
              Manage libvirt machines, installation media, and Fedora deployment
              jobs from one host.
            </p>
          </section>

          <section className='border border-border bg-card p-6 sm:p-8'>
            <div className='mb-8 flex items-center justify-between'>
              <div>
                <h2 className='text-lg font-medium tracking-tight'>
                  API access
                </h2>
                <p className='mt-1 text-sm text-muted-foreground'>
                  Session-scoped credentials
                </p>
              </div>
              <span className='flex items-center gap-2 font-mono text-[0.6875rem] text-muted-foreground uppercase'>
                <span
                  className={`size-1.5 rounded-full ${health.data?.ok ? 'bg-status-success' : 'bg-status-warning'}`}
                />
                {health.isPending
                  ? 'Checking'
                  : health.data?.ok
                    ? 'Online'
                    : 'Unavailable'}
              </span>
            </div>

            <form className='grid gap-5' onSubmit={submit}>
              <div className='grid gap-2'>
                <Label htmlFor='access-token'>Bearer token</Label>
                <Input
                  id='access-token'
                  type='password'
                  autoComplete='off'
                  value={token}
                  onChange={(event) => setToken(event.target.value)}
                  className='h-11 font-mono'
                  autoFocus
                />
              </div>
              {error && (
                <p className='flex items-center gap-2 text-sm text-destructive'>
                  <CircleAlert className='size-4' />
                  {error}
                </p>
              )}
              <Button
                type='submit'
                className='h-11 justify-between'
                disabled={submitting}
              >
                {submitting ? 'Verifying' : 'Continue'}
                <ArrowRight className='size-4' />
              </Button>
            </form>

            <p className='mt-6 border-t border-border pt-5 text-xs leading-5 text-muted-foreground'>
              Stored only for this browser session.
            </p>
          </section>
        </div>

        <footer className='flex items-center justify-between border-t border-border pt-5 font-mono text-[0.6875rem] text-muted-foreground uppercase'>
          <span>qemu / libvirt</span>
          <span>{health.data?.version ?? '—'}</span>
        </footer>
      </div>
    </main>
  )
}
