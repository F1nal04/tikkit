import { API_ORIGIN } from '$app/env/private'
import { resolveLocale } from '#lib/i18n.ts'
import type { Cookies } from '@sveltejs/kit'
import type { Handle, HandleFetch } from '@sveltejs/kit/hooks'

export const handle: Handle = ({ event, resolve }) => {
  const query = event.url.searchParams.get('lang')
  const locale = resolveLocale(
    query,
    event.cookies.get('lang'),
    event.request.headers.get('accept-language'),
  )
  if (query === locale) {
    event.cookies.set('lang', locale, { path: '/', sameSite: 'lax', maxAge: 31536000 })
  }
  event.locals.locale = locale
  // Pages are rendered per user and per language; never let caches share them.
  event.setHeaders({ 'cache-control': 'private, no-store', vary: 'Cookie, Accept-Language' })
  let done = false
  return resolve(event, {
    transformPageChunk: ({ html }) => {
      if (done || !html.includes('<html')) return html
      done = true
      return html.replace('%lang%', locale)
    },
  })
}

// During SSR, `/api` is not served by this process; send it to Rust directly.
export const handleFetch: HandleFetch = async ({ event, request, fetch }) => {
  const url = new URL(request.url)
  if (url.origin !== event.url.origin || !url.pathname.startsWith('/api/')) return fetch(request)
  request = new Request(new URL(url.pathname + url.search, API_ORIGIN), request)
  for (const name of ['accept-language', 'x-forwarded-for']) {
    const value = event.request.headers.get(name)
    if (value) request.headers.set(name, value)
  }
  // Only the session cookie, read through `event.cookies` so fetches later in the
  // same render use a token rotated (and relayed below) by an earlier one.
  const session = event.cookies.get('tikkit_session')
  if (session) request.headers.set('cookie', `tikkit_session=${session}`)
  else request.headers.delete('cookie')
  const response = await fetch(request)
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
