import { redirect } from '@sveltejs/kit'
import { type Account, createClient } from '@tikkit/api-client'
import { pageLocale } from '#lib/i18n.ts'
import type { LayoutServerLoad } from './$types'

/** Pages reachable without a session. Other pages redirect to /login. */
const PUBLIC = new Set(['/', '/about', '/login', '/setup', '/forgot-password'])
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
  if (user && path === '/login') redirect(303, '/')
  const isPublic = PUBLIC.has(path) || PUBLIC_PREFIXES.some((p) => path.startsWith(p))
  if (!user && !setupRequired && !isPublic) redirect(303, '/login')
  locals.locale = pageLocale(locals.locale, url.searchParams.get('lang'), user?.language)
  return { locale: locals.locale, user }
}
