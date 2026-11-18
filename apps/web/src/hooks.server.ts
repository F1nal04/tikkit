import { randomUUID } from 'node:crypto'
import { API_ORIGIN } from '$app/env/private'
import { resolveLocale } from '#lib/i18n.ts'
import type { Cookies, RequestEvent } from '@sveltejs/kit'
import type { Handle, HandleFetch, HandleServerError } from '@sveltejs/kit/hooks'

export const handle: Handle = async ({ event, resolve }) => {
  const start = performance.now()
  event.locals.requestId = randomUUID()
  const query = event.url.searchParams.get('lang')
  const locale = resolveLocale(
    query,
    event.cookies.get('lang'),
    event.request.headers.get('accept-language'),
  )
  if (query === locale) {
    // The browser language selector updates this preference cookie.
    event.cookies.set('lang', locale, { path: '/', httpOnly: false, sameSite: 'lax', maxAge: 31536000 })
  }
  event.locals.locale = locale
  // Pages are rendered per user and per language; never let caches share them.
  // no-referrer: invite and reset page URLs carry tokens.
  event.setHeaders({
    'cache-control': 'private, no-store',
    'x-request-id': event.locals.requestId,
    vary: 'Cookie, Accept-Language',
    ...securityHeaders(event.url),
  })
  let done = false
  const response = await resolve(event, {
    transformPageChunk: ({ html }) => {
      if (done || !html.includes('<html')) return html
      done = true
      // The layout load may switch to the signed-in account's language.
      return html.replace('%lang%', event.locals.locale)
    },
  })
  log(event, 'info', 'web_request_completed', response.status, Math.round(performance.now() - start))
  return response
}

/**
 * Security headers on every rendered page (docs/security.md). The CSP itself
 * comes from SvelteKit (`csp` in vite.config.ts) because it carries the nonce.
 * HSTS only when the portal is served over https (`ORIGIN`).
 */
export function securityHeaders(url: URL): Record<string, string> {
  const headers: Record<string, string> = {
    'referrer-policy': 'no-referrer',
    'x-content-type-options': 'nosniff',
    'x-frame-options': 'DENY',
    'cross-origin-opener-policy': 'same-origin',
    'cross-origin-resource-policy': 'same-origin',
    'permissions-policy': 'camera=(), microphone=(), geolocation=(), payment=(), usb=()',
  }
  if (url.protocol === 'https:') headers['strict-transport-security'] = 'max-age=31536000'
  return headers
}

export const handleError: HandleServerError = ({ event, kind, error }) => {
  const status = kind === 'unknown' ? 500 : error.status
  if (typeof status === 'number' && status >= 500) log(event, 'error', 'ssr_failed', status)
}

function log(
  event: RequestEvent,
  level: 'info' | 'error',
  code: 'web_request_completed' | 'ssr_failed' | 'api_unreachable' | 'api_request_failed',
  status?: number,
  latency_ms?: number,
) {
  // Use route templates and fixed fields. URLs, exceptions and headers may contain secrets.
  console[level](JSON.stringify({
    timestamp: new Date().toISOString(),
    service: 'web',
    level,
    event: code,
    request_id: event.locals.requestId,
    route: event.route.id ?? 'unmatched',
    status,
    latency_ms,
  }))
}

// During SSR, `/api` is not served by this process; send it to Rust directly.
export const handleFetch: HandleFetch = async ({ event, request, fetch }) => {
  const url = new URL(request.url)
  if (url.origin !== event.url.origin || !url.pathname.startsWith('/api/')) return fetch(request)
  request = new Request(new URL(url.pathname + url.search, API_ORIGIN), request)
  request.headers.set('x-request-id', event.locals.requestId)
  for (const name of ['accept-language', 'x-forwarded-for']) {
    const value = event.request.headers.get(name)
    if (value) request.headers.set(name, value)
  }
  // Only the session cookie, read through `event.cookies` so fetches later in the
  // same render use a token rotated (and relayed below) by an earlier one.
  const session = event.cookies.get('tikkit_session')
  if (session) request.headers.set('cookie', `tikkit_session=${session}`)
  else request.headers.delete('cookie')
  const response = await fetch(request).catch((error: unknown) => {
    log(event, 'error', 'api_unreachable')
    throw error
  })
  if (response.status >= 500) log(event, 'error', 'api_request_failed', response.status)
  // SvelteKit drops Set-Cookie from cross-origin fetches; relay a rotated session to the browser.
  for (const header of response.headers.getSetCookie()) relaySessionCookie(event.cookies, header)
  return response
}

function relaySessionCookie(cookies: Cookies, header: string) {
  const [pair, ...attributes] = header.split(';').map((part) => part.trim())
  const split = pair.indexOf('=')
  if (pair.slice(0, split) !== 'tikkit_session') return
  const attribute = (name: string) =>
    attributes.find((a) => a.toLowerCase().startsWith(name))?.slice(name.length)
  cookies.set('tikkit_session', pair.slice(split + 1), {
    path: '/',
    httpOnly: true,
    sameSite: 'lax',
    secure: attribute('secure') !== undefined,
    maxAge: Number(attribute('max-age=') ?? 0),
    encode: (value) => value,
  })
}
