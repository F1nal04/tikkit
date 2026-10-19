import { ApiError, createClient } from '@tikkit/api-client'
import { parseQueue, queueQuery } from '#lib/queue.ts'
import type { PageLoad } from './$types'

export const load: PageLoad = async ({ fetch, parent, url }) => {
  // The (app) layout rejects non-staff first; the API answers 403 to them regardless.
  const { user } = await parent()
  const params = parseQueue(url.searchParams, user!.role)
  const api = createClient({ fetch })
  try {
    const [page, counts, catalog] = await Promise.all([
      api.request('get', '/api/queue', { query: { ...queueQuery(params), limit: 25 } }),
      api.request('get', '/api/queue/counts'),
      api.request('get', '/api/catalog'),
    ])
    return { params, page, counts, catalog, unavailable: false as const }
  } catch (e) {
    // An unreachable or failing API keeps the page and offers a retry; 401/403/404 are real answers.
    if (e instanceof ApiError && (e.status === 0 || e.status >= 500)) {
      return { params, page: null, counts: null, catalog: null, unavailable: true as const }
    }
    throw e
  }
}
