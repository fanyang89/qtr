import { useQuery } from '@tanstack/react-query'
import { getHealth } from '@/lib/api'
import {
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
} from '@/components/ui/sidebar'

export function NavUser() {
  const health = useQuery({
    queryKey: ['health'],
    queryFn: getHealth,
    refetchInterval: 30_000,
  })
  const online = health.data?.ok === true
  return (
    <SidebarMenu>
      <SidebarMenuItem>
        <SidebarMenuButton size='lg' tooltip='Host status'>
          <span
            className={`ms-1 size-2 rounded-full ${online ? 'bg-status-success' : 'bg-status-warning'}`}
          />
          <div className='grid flex-1 text-start leading-tight'>
            <span className='truncate text-xs font-medium'>Local host</span>
            <span className='truncate font-mono text-[0.625rem] text-muted-foreground uppercase'>
              {health.isPending
                ? 'checking'
                : online
                  ? 'available'
                  : 'degraded'}
            </span>
          </div>
        </SidebarMenuButton>
      </SidebarMenuItem>
    </SidebarMenu>
  )
}
