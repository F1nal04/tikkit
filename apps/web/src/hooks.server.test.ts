import { describe, expect, it, vi } from 'vitest'
import type { RequestEvent } from '@sveltejs/kit'
import { handle, handleFetch } from './hooks.server'

vi.mock('$app/env/private', () => ({ API_ORIGIN: 'http://api.internal' }))

describe('SSR correlation', () => {
  it('shares a fresh request ID with same-origin API calls and safe request logs', async () => {
    const privateValue = 'private-sign-in-token'
    const log = vi.spyOn(console, 'info').mockImplementation(() => {})
    const url = new URL(`https://portal.example/invite/${privateValue}?token=${privateValue}`)
    const setHeaders = vi.fn()
    const event = {
      url,
      request: new Request(url, { headers: { 'x-request-id': privateValue } }),
      route: { id: '/invite/[token]' },
      locals: {},
      cookies: { get: () => undefined },
      setHeaders,
    } as unknown as RequestEvent
    const fetch = vi.fn(async (_request: RequestInfo | URL) => new Response())
    try {
      await handle({
        event,
        resolve: async () =>
          handleFetch({ event, request: new Request(new URL('/api/setup', url)), fetch }),
      })
      const id = setHeaders.mock.calls[0][0]['x-request-id']
      expect(id).toMatch(/^[\da-f-]{36}$/)
      const upstream = new Request(fetch.mock.calls[0][0])
      expect(upstream.url).toBe('http://api.internal/api/setup')
      expect(upstream.headers.get('x-request-id')).toBe(id)
      const output = JSON.parse(log.mock.calls[0][0])
      expect(output).toMatchObject({ request_id: id, route: '/invite/[token]', status: 200 })
      expect(JSON.stringify(output)).not.toContain(privateValue)
    } finally {
      log.mockRestore()
    }
  })
})
