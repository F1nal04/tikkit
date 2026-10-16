import { redirect } from '@sveltejs/kit'
import { type Account, createClient } from '@tikkit/api-client'
import type { LayoutServerLoad } from './$types'

/** Pages reachable without a session. Other pages redirect to /login. */
const PUBLIC = new Set(['/', '/about', '/login', '/setup'])

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
  if (!user && !setupRequired && !PUBLIC.has(path)) redirect(303, '/login')
  return { locale: locals.locale, user }
}
