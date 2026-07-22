import { describe, expect, test } from 'vitest'
import vmStatesJson from '../../../fixtures/vm-states.json?raw'
import { vmStateSchema } from './api'

describe('VM API contract', () => {
  test('accepts every backend VM state', () => {
    const states = JSON.parse(vmStatesJson) as unknown

    expect(vmStateSchema.options).toEqual(states)
    expect(vmStateSchema.array().parse(states)).toEqual([
      'nostate',
      'running',
      'blocked',
      'paused',
      'shutdown',
      'shutoff',
      'crashed',
      'pmsuspended',
      'unknown',
    ])
  })
})
