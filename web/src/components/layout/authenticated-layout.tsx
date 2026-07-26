import { Outlet } from '@tanstack/react-router'
import { getCookie } from '@/lib/cookies'
import { SearchProvider } from '@/context/search-provider'
import { SidebarInset, SidebarProvider } from '@/components/ui/sidebar'
import { AppSidebar } from '@/components/layout/app-sidebar'
import { SkipToMain } from '@/components/skip-to-main'

type AuthenticatedLayoutProps = {
  children?: React.ReactNode
}

export function AuthenticatedLayout({ children }: AuthenticatedLayoutProps) {
  const defaultOpen = getCookie('sidebar_state') !== 'false'
  return (
    <SearchProvider>
      <SidebarProvider defaultOpen={defaultOpen}>
        <SkipToMain />
        <AppSidebar />
        <SidebarInset className='@container/content bg-background has-data-[layout=fixed]:h-svh'>
          {children ?? <Outlet />}
        </SidebarInset>
      </SidebarProvider>
    </SearchProvider>
  )
}
