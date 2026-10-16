import { API_ORIGIN } from '$app/env/private'
import { resolveLocale } from '#lib/i18n.ts'
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
  return resolve(event, {
    transformPageChunk: ({ html }) => html.replace('%lang%', locale),
  })
}

// During SSR, `/api` is not served by this process; send it to Rust directly.
export const handleFetch: HandleFetch = ({ event, request, fetch }) => {
  const url = new URL(request.url)
  if (url.origin === event.url.origin && url.pathname.startsWith('/api/')) {
    request = new Request(new URL(url.pathname + url.search, API_ORIGIN), request)
    const cookie = event.request.headers.get('cookie')
    if (cookie) request.headers.set('cookie', cookie)
  }
  return fetch(request)
}
