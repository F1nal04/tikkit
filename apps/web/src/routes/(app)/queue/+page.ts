import { ApiError, createClient } from '@tikkit/api-client'
import { parseQueue, queueQuery } from '#lib/queue.ts'
import type { PageLoad } from './$types'

export const load: PageLoad = async ({ fetch, parent, url }) => {
  // The (app) layout rejects non-staff first; the API answers 403 to them regardless.
  const { user } = await parent()
  const params = parseQueue(url.searchParams, user!.role)
  const api = createClient({ fetch })
  try {
    // Counts first: their `version` is then never newer than the list, so polling
    // catches every change made after this load.
    const counts = await api.request('get', '/api/queue/counts')
    const [{ page, searchError }, catalog, views, options] = await Promise.all([
      api.request('get', '/api/queue', { query: { ...queueQuery(params), limit: 25 } }).then(
        (page) => ({ page, searchError: null }),
        (e) => {
          // Search text out of bounds: keep the page and say so at the search box.
          const code = e instanceof ApiError ? e.fields?.q?.[0] : undefined
          if (code) return { page: { items: [], next_cursor: null }, searchError: code }
          throw e
        },
      ),
      api.request('get', '/api/catalog'),
      api.request('get', '/api/saved-views'),
      api.request('get', '/api/queue/bulk-options'),
    ])
    return { params, page, counts, catalog, views, options, searchError, unavailable: false as const }
  } catch (e) {
    // An unreachable or failing API keeps the page and offers a retry; 401/403/404 are real answers.
    if (e instanceof ApiError && (e.status === 0 || e.status >= 500)) {
      return { params, page: null, counts: null, catalog: null, views: null, options: null, searchError: null, unavailable: true as const }
    }
    throw e
  }
}
