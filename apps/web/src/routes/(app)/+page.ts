import { ApiError, createClient } from '@tikkit/api-client'
import { error } from '@sveltejs/kit'
import { historyFilters, historyQuery } from '#lib/requester.ts'
import type { PageLoad } from './$types'

export const load: PageLoad = async ({ fetch, parent, url }) => {
  await parent()
  const filters = historyFilters(url.searchParams)
  try {
    const page = await createClient({ fetch }).request('get', '/api/tickets', { query: historyQuery(filters) })
    return { filters, tickets: page, unavailable: false }
  } catch (e) {
    if (e instanceof ApiError) {
      if (e.status === 0 || e.status >= 500) return { filters, tickets: null, unavailable: true }
      error(e.status, e.code)
    }
    throw e
  }
}
