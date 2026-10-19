import { redirect } from '@sveltejs/kit'
import { type Account, createClient } from '@tikkit/api-client'
import { pageLocale } from '#lib/i18n.ts'
import { safeNext } from '#lib/redirect.ts'
import type { LayoutServerLoad } from './$types'

/** Pages reachable without a session. Other pages redirect to /login. */
const PUBLIC = new Set(['/login', '/setup', '/forgot-password'])
const PUBLIC_PREFIXES = ['/invite/', '/reset-password/']

// Navigation aid only: the API enforces authentication and setup state itself.
export const load: LayoutServerLoad = async ({ locals, fetch, url }) => {
  const api = createClient({ fetch, language: locals.locale })
  const user: Account | null = await api.request('get', '/api/auth/me').catch(() => null)
  const setupRequired =
    !user &&
    (await api
      .request('get', '/api/setup')
      .then((s) => s.setup_required)
      .catch(() => false))
  const path = url.pathname
  if (setupRequired && path !== '/setup') redirect(303, '/setup')
  if (!setupRequired && path === '/setup') redirect(303, '/')
  if (user && path === '/login') redirect(303, safeNext(url.searchParams.get('next')) ?? '/')
  const isPublic = PUBLIC.has(path) || PUBLIC_PREFIXES.some((p) => path.startsWith(p))
  if (!user && !setupRequired && !isPublic) {
    // Remember where the visitor was headed so signing in returns there.
    const next = safeNext(path + url.search)
    redirect(303, next && next !== '/' ? `/login?next=${encodeURIComponent(next)}` : '/login')
  }
  locals.locale = pageLocale(locals.locale, url.searchParams.get('lang'), user?.language)
  const organization = user ? await api.request('get', '/api/organization').catch(() => null) : null
  const unread = user
    ? await api.request('get', '/api/notifications/unread').then((r) => r.count).catch(() => 0)
    : 0
  return { locale: locals.locale, user, ...(unread ? { unread } : {}), ...(organization ? { organization } : {}) }
}
