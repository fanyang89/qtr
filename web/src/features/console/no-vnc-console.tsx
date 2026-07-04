import { useEffect, useRef, useState } from 'react'
import RFB from '@novnc/novnc'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Switch } from '@/components/ui/switch'
import { Label } from '@/components/ui/label'
import { Separator } from '@/components/ui/separator'
import { buildVncWebSocketUrl } from '@/lib/vnc'

type ConnectionState = 'idle' | 'connecting' | 'connected' | 'disconnected' | 'error'

export function NoVncConsole({ name }: { name: string }) {
  const containerRef = useRef<HTMLDivElement>(null)
  const rfbRef = useRef<RFB | null>(null)
  const [connectionState, setConnectionState] = useState<ConnectionState>('idle')
  const [errorMessage, setErrorMessage] = useState<string | null>(null)
  const [desktopName, setDesktopName] = useState<string | null>(null)
  const [hasClient, setHasClient] = useState(false)
  const [viewOnly, setViewOnly] = useState(false)
  const [scaleViewport, setScaleViewport] = useState(true)
  const [resizeSession, setResizeSession] = useState(true)
  const webSocketUrl = buildVncWebSocketUrl(name)

  useEffect(() => {
    const rfb = rfbRef.current
    if (!rfb) return
    rfb.viewOnly = viewOnly
    rfb.scaleViewport = scaleViewport
    rfb.resizeSession = resizeSession
  }, [resizeSession, scaleViewport, viewOnly])

  function connect() {
    if (!containerRef.current || rfbRef.current) return

    setConnectionState('connecting')
    setErrorMessage(null)
    setDesktopName(null)

    let rfb: RFB
    try {
      rfb = new RFB(containerRef.current, webSocketUrl, {
        shared: true,
      })
    } catch (error) {
      setConnectionState('error')
      setErrorMessage(error instanceof Error ? error.message : 'Failed to create noVNC client')
      return
    }

    rfbRef.current = rfb
    setHasClient(true)
    rfb.viewOnly = viewOnly
    rfb.scaleViewport = scaleViewport
    rfb.resizeSession = resizeSession
    rfb.focusOnClick = true
    rfb.qualityLevel = 8
    rfb.compressionLevel = 2

    rfb.addEventListener('connect', () => {
      setConnectionState('connected')
      setErrorMessage(null)
    })
    rfb.addEventListener('disconnect', (event) => {
      const clean = 'detail' in event && Boolean((event as CustomEvent<{ clean?: boolean }>).detail.clean)
      setConnectionState(clean ? 'disconnected' : 'error')
      setErrorMessage(clean ? null : 'VNC WebSocket closed before the desktop connected')
      rfbRef.current = null
      setHasClient(false)
    })
    rfb.addEventListener('desktopname', (event) => {
      const detail = (event as CustomEvent<{ name?: string }>).detail
      setDesktopName(detail.name ?? null)
    })
    rfb.addEventListener('credentialsrequired', () => {
      setConnectionState('error')
      setErrorMessage('VNC requires credentials')
    })
    rfb.addEventListener('securityfailure', (event) => {
      const detail = (event as CustomEvent<{ reason?: string }>).detail
      setConnectionState('error')
      setErrorMessage(detail.reason ?? 'VNC security negotiation failed')
    })
  }

  function disconnect() {
    rfbRef.current?.disconnect()
    rfbRef.current = null
    setHasClient(false)
    setConnectionState('disconnected')
  }

  useEffect(() => {
    // noVNC connection inherently needs to start inside the effect and reports
    // state through listeners; the synchronous setState here is intentional.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    connect()
    return () => {
      rfbRef.current?.disconnect()
      rfbRef.current = null
      setHasClient(false)
    }
    // The initial connection owns the RFB instance lifecycle for this VM route.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [name])

  return (
    <div className='flex h-full min-h-[calc(100svh-4rem)] flex-col bg-muted/30'>
      <div className='flex flex-wrap items-center gap-3 border-b bg-background px-4 py-3'>
        <Badge variant={connectionState === 'connected' ? 'default' : 'secondary'}>{connectionState}</Badge>
        {desktopName && <Badge variant='outline'>{desktopName}</Badge>}
        <Separator orientation='vertical' className='h-6' />
        <Button size='sm' onClick={connect} disabled={connectionState === 'connected' || connectionState === 'connecting'}>Connect</Button>
        <Button size='sm' variant='outline' onClick={disconnect} disabled={!hasClient}>Disconnect</Button>
        <Button size='sm' variant='outline' onClick={() => rfbRef.current?.sendCtrlAltDel()} disabled={!hasClient}>Ctrl Alt Del</Button>
        <Toggle label='View only' checked={viewOnly} onCheckedChange={setViewOnly} />
        <Toggle label='Scale' checked={scaleViewport} onCheckedChange={setScaleViewport} />
        <Toggle label='Resize' checked={resizeSession} onCheckedChange={setResizeSession} />
      </div>
      <div className='border-b bg-background px-4 py-2 text-xs text-muted-foreground'>
        <span className='font-medium'>WebSocket:</span> {webSocketUrl}
        {errorMessage && <span className='ms-3 text-destructive'>{errorMessage}</span>}
      </div>
      <div ref={containerRef} className='min-h-0 flex-1 overflow-hidden bg-black' />
    </div>
  )
}

function Toggle({ label, checked, onCheckedChange }: { label: string; checked: boolean; onCheckedChange: (checked: boolean) => void }) {
  return (
    <div className='flex items-center gap-2'>
      <Switch checked={checked} onCheckedChange={onCheckedChange} id={label} />
      <Label htmlFor={label} className='text-sm'>{label}</Label>
    </div>
  )
}
