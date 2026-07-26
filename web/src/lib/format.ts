export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  const units = ['KiB', 'MiB', 'GiB', 'TiB']
  let value = bytes
  let unit = -1
  do {
    value /= 1024
    unit += 1
  } while (value >= 1024 && unit < units.length - 1)
  return `${new Intl.NumberFormat('en', { maximumFractionDigits: value < 10 ? 1 : 0 }).format(value)} ${units[unit]}`
}

export function formatTimestamp(timestamp?: number | null): string {
  if (!timestamp) return '—'
  return new Intl.DateTimeFormat('en', {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(timestamp)
}

export function formatDuration(
  start?: number | null,
  end?: number | null
): string {
  if (!start) return '—'
  const seconds = Math.max(0, Math.round(((end ?? Date.now()) - start) / 1000))
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  const remainder = seconds % 60
  if (minutes < 60) return `${minutes}m ${remainder}s`
  const hours = Math.floor(minutes / 60)
  return `${hours}h ${minutes % 60}m`
}
