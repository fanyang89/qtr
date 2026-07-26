import { describe, expect, test } from 'vitest'
import { buildVncWebSocketUrl } from './vnc'

describe('VNC WebSocket URL', () => {
  test('uses the versioned API and URL-encodes the ticket', () => {
    const url = new URL(buildVncWebSocketUrl('vm/name', 'ticket + value'))

    expect(url.protocol).toBe(
      window.location.protocol === 'https:' ? 'wss:' : 'ws:'
    )
    expect(url.pathname).toBe('/api/v1/vms/vm%2Fname/vnc')
    expect(url.searchParams.get('ticket')).toBe('ticket + value')
  })
})
