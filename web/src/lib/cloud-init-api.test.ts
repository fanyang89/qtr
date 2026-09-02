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
    expect(() =>
      cloudInitSeedInputSchema.parse({ ...input, instanceId: '' })
    ).toThrow()
  })

  test('posts the camel-case seed request and parses managed media', async () => {
    axiosMocks.post.mockResolvedValue({ data: response })

    await expect(createCloudInitSeed(input)).resolves.toEqual(response)
    expect(axiosMocks.post).toHaveBeenCalledWith('/media/cloud-init', input)
  })
})
