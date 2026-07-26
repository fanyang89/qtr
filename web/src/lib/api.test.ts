import { describe, expect, test } from 'vitest'
import vmStatesJson from '../../../fixtures/vm-states.json?raw'
import {
  API_TOKEN_STORAGE_KEY,
  getApiToken,
  installJobSchema,
  managedResourceSchema,
  setApiToken,
  vmSummarySchema,
  vmStateSchema,
} from './api'

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

  test('stores the API token in session storage', () => {
    setApiToken('secret')

    expect(getApiToken()).toBe('secret')
    expect(sessionStorage.getItem(API_TOKEN_STORAGE_KEY)).toBe('secret')

    setApiToken('')
    expect(getApiToken()).toBe('')
    expect(sessionStorage.getItem(API_TOKEN_STORAGE_KEY)).toBeNull()
  })

  test('parses backend memory fields from the OpenAPI contract', () => {
    const vm = vmSummarySchema.parse({
      name: 'fedora',
      state: 'running',
      id: '1',
      vnc: true,
      memoryMib: 4096,
      graphics: 'vnc',
      metrics: {
        cpuTimeNs: 10,
        memoryUsedMib: 1024,
        memoryTotalMib: 4096,
        txBytes: 20,
        rxBytes: 30,
        sampledAtMs: 40,
      },
    })

    expect(vm.memoryMib).toBe(4096)
    expect(vm.metrics?.memoryUsedMib).toBe(1024)
  })

  test('parses install jobs and managed resources', () => {
    const request = {
      name: 'fedora',
      mediaId: 'Fedora.iso',
      imageId: 'fedora.qcow2',
      sshAuthorizedKey: 'ssh-ed25519 AAAA',
      diskSize: '40GiB',
      memoryMib: 4096,
      vcpus: 2,
      network: 'default',
      hostname: null,
      mirror: 'official',
      timeoutSecs: 7200,
      verifyTimeoutSecs: 300,
      keepFailed: false,
    }
    expect(
      installJobSchema.parse({
        id: 'job-id',
        status: 'running',
        phase: 'installing',
        cancelRequested: false,
        request,
        error: null,
        createdAtMs: 1,
        startedAtMs: 2,
        finishedAtMs: null,
      }).request.imageId
    ).toBe('fedora.qcow2')
    expect(
      managedResourceSchema.parse({
        id: 'Fedora.iso',
        sizeBytes: 42,
        modifiedAtMs: null,
      }).modifiedAtMs
    ).toBeNull()
  })
})
