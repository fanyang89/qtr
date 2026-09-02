import { beforeEach, describe, expect, test, vi } from 'vitest'
import { cloudInitSeedInputSchema, createCloudInitSeed } from './api'

const axiosMocks = vi.hoisted(() => ({
  post: vi.fn(),
  requestInterceptor: vi.fn(),
  responseInterceptor: vi.fn(),
}))

vi.mock('axios', () => ({
  default: {
    create: () => ({
      post: axiosMocks.post,
      interceptors: {
        request: { use: axiosMocks.requestInterceptor },
        response: { use: axiosMocks.responseInterceptor },
      },
    }),
    isAxiosError: () => false,
  },
}))

const input = {
  id: 'node-1-seed.iso',
  instanceId: 'node-1',
  localHostname: 'node-1',
  userData: '',
  networkConfig: 'version: 2\n',
}

const response = {
  id: input.id,
  sizeBytes: 36_864,
  modifiedAtMs: 1,
  status: 'ready',
  attachments: [],
  reservedByJobIds: [],
}

describe('cloud-init seed API', () => {
  beforeEach(() => {
    axiosMocks.post.mockReset()
  })

  test('accepts empty user data and optional NoCloud content', () => {
    expect(cloudInitSeedInputSchema.parse(input)).toEqual(input)
  })

  test('enforces server ID, hostname, UTF-8 byte, and aggregate limits', () => {
    for (const invalid of [
      { ...input, id: '../node.iso' },
      { ...input, id: '.hidden.iso' },
      { ...input, instanceId: ' ' },
      { ...input, instanceId: 'node\0one' },
      { ...input, instanceId: 'é'.repeat(128) },
      { ...input, localHostname: 'bad_host' },
      { ...input, localHostname: '-node' },
      { ...input, userData: '🙂'.repeat(262_145) },
      {
        ...input,
        networkConfig: 'x'.repeat(1024 * 1024),
        vendorData: 'x'.repeat(1024 * 1024),
      },
    ]) {
      expect(() => cloudInitSeedInputSchema.parse(invalid)).toThrow()
    }
  })

  test('posts the camel-case seed request and parses managed media', async () => {
    axiosMocks.post.mockResolvedValue({ data: response })

    await expect(createCloudInitSeed(input)).resolves.toEqual(response)
    expect(axiosMocks.post).toHaveBeenCalledWith('/media/cloud-init', input)
  })

  test('rejects invalid input before sending a request', async () => {
    await expect(
      createCloudInitSeed({ ...input, localHostname: 'bad_host' })
    ).rejects.toThrow()
    expect(axiosMocks.post).not.toHaveBeenCalled()
  })
})
