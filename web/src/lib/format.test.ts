import { describe, expect, test } from 'vitest'
import { formatBytes, formatDuration, formatTimestamp } from './format'

describe('formatters', () => {
  test('formats binary byte sizes', () => {
    expect(formatBytes(42)).toBe('42 B')
    expect(formatBytes(1024)).toBe('1 KiB')
    expect(formatBytes(5 * 1024 * 1024)).toBe('5 MiB')
  })

  test('formats durations without requiring a finish time', () => {
    expect(formatDuration(null, null)).toBe('—')
    expect(formatDuration(0, 1000)).toBe('—')
    expect(formatDuration(1000, 66_000)).toBe('1m 5s')
  })

  test('uses an em dash for absent timestamps', () => {
    expect(formatTimestamp(null)).toBe('—')
  })
})
