import { ApiError, createClient } from '@tikkit/api-client'
import type { PageLoad } from './$types'

export const load: PageLoad = async ({ fetch, parent }) => {
  await parent()
  try {
    const page = await createClient({ fetch }).request('get', '/api/notifications', { query: { limit: 25 } })
    return { page, unavailable: false as const }
  } catch (e) {
    if (e instanceof ApiError && (e.status === 0 || e.status >= 500)) return { page: null, unavailable: true as const }
    throw e
  }
}
