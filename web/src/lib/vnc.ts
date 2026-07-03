export function buildVncWebSocketUrl(name: string) {
  const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
  const host = window.location.host
  return `${protocol}//${host}/api/vms/${encodeURIComponent(name)}/vnc`
}
