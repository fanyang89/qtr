export function buildVncWebSocketUrl(name: string, ticket: string) {
  const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
  const host = window.location.host
  const query = new URLSearchParams({ ticket })
  return `${protocol}//${host}/api/v1/vms/${encodeURIComponent(name)}/vnc?${query}`
}
