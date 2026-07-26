import { cn } from '@/lib/utils'

const tones: Record<string, string> = {
  running: 'bg-status-success',
  succeeded: 'bg-status-success',
  queued: 'bg-muted-foreground',
  failed: 'bg-destructive',
  crashed: 'bg-destructive',
  interrupted: 'bg-status-warning',
  cancelled: 'bg-muted-foreground',
  shutdown: 'bg-muted-foreground',
  shutoff: 'bg-muted-foreground',
}

export function StatusDot({
  status,
  className,
}: {
  status: string
  className?: string
}) {
  return (
    <span
      className={cn(
        'inline-flex items-center gap-2 text-sm capitalize',
        className
      )}
    >
      <span
        className={cn(
          'size-1.5 rounded-full',
          tones[status] ?? 'bg-status-warning'
        )}
      />
      {status}
    </span>
  )
}
