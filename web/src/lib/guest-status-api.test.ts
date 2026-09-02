import { beforeEach, describe, expect, test, vi } from 'vitest'
import { getVmGuestStatus, vmGuestStatusSchema } from './api'

const axiosMocks = vi.hoisted(() => ({
  get: vi.fn(),
  requestInterceptor: vi.fn(),
  responseInterceptor: vi.fn(),
}))

vi.mock('axios', () => ({
  default: {
    create: () => ({
      get: axiosMocks.get,
      interceptors: {
        request: { use: axiosMocks.requestInterceptor },
        response: { use: axiosMocks.responseInterceptor },
      },
    }),
    isAxiosError: () => false,
  },
}))

const response = {
  name: 'node-1',
  domainState: 'running',
  guestAgentReady: true,
  networkInterfacesAvailable: true,
  interfaces: [
    {
      name: 'eth0',
      hardwareAddress: '52:54:00:12:34:56',
      addresses: [
        {
          type: 'ipv4',
          address: '192.0.2.10',
          prefix: 24,
          usable: true,
        },
      ],
    },
  ],
  observedAtMs: 1,
}

describe('VM guest status API', () => {
  beforeEach(() => {
    axiosMocks.get.mockReset()
  })

  test('parses discovered interfaces and unavailable observations', () => {
    expect(vmGuestStatusSchema.parse(response)).toEqual(response)
    expect(
      vmGuestStatusSchema.parse({
        ...response,
        domainState: 'shutoff',
        guestAgentReady: false,
        networkInterfacesAvailable: false,
        interfaces: [],
      }).networkInterfacesAvailable
    ).toBe(false)
  })

  test('gets the encoded VM guest-status route', async () => {
    axiosMocks.get.mockResolvedValue({ data: response })

    await expect(getVmGuestStatus('node/1')).resolves.toEqual(response)
    expect(axiosMocks.get).toHaveBeenCalledWith('/vms/node%2F1/guest-status')
  })

  test('rejects malformed response data', async () => {
    axiosMocks.get.mockResolvedValue({
      data: { ...response, guestAgentReady: 'yes' },
    })

    await expect(getVmGuestStatus('node-1')).rejects.toThrow()
  })
})
