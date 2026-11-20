import { describe, expect, it, vi } from 'vitest'
import type { RequestEvent } from '@sveltejs/kit'
import { handle, handleFetch, securityHeaders } from './hooks.server'

vi.mock('$app/env/private', () => ({ API_ORIGIN: 'http://api.internal', PROTOCOL_HEADER: '' }))

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

describe('page security headers', () => {
  it('forbid framing and sniffing everywhere and send HSTS only for proxy-confirmed https', () => {
    for (const origin of ['http://127.0.0.1:3000', 'https://help.example.com']) {
      for (const fromProxy of [false, true]) {
        const headers = securityHeaders(new URL('/tickets', origin), fromProxy)
        expect(headers).toMatchObject({
          'referrer-policy': 'no-referrer',
          'x-content-type-options': 'nosniff',
          'x-frame-options': 'DENY',
          'cross-origin-opener-policy': 'same-origin',
        })
        // Without PROTOCOL_HEADER the https URL is only adapter-node's assumption.
        expect('strict-transport-security' in headers).toBe(fromProxy && origin.startsWith('https:'))
      }
    }
  })

  it('forward only the session cookie and never a client-chosen request ID to the API', async () => {
    const url = new URL('https://help.example.com/tickets')
    const event = {
      url,
      request: new Request(url, {
        headers: { cookie: 'tikkit_session=abc; other=secret', 'x-request-id': 'forged', host: 'evil' },
      }),
      locals: { requestId: '11111111-1111-4111-8111-111111111111' },
      cookies: { get: (name: string) => (name === 'tikkit_session' ? 'abc' : undefined) },
    } as unknown as RequestEvent
    const fetch = vi.fn(async (_request: RequestInfo | URL) => new Response())
    await handleFetch({ event, request: new Request(new URL('/api/tickets', url)), fetch })
    const upstream = new Request(fetch.mock.calls[0][0])
    expect(upstream.headers.get('cookie')).toBe('tikkit_session=abc')
    expect(upstream.headers.get('x-request-id')).toBe('11111111-1111-4111-8111-111111111111')
    // Cross-origin fetches during SSR are passed through untouched, without the session.
    await handleFetch({ event, request: new Request('https://elsewhere.example/api/x'), fetch })
    expect(new Request(fetch.mock.calls[1][0]).headers.get('cookie')).toBeNull()
  })
})
