import { ApiError, createClient } from '@tikkit/api-client'
import { error, redirect } from '@sveltejs/kit'
import type { PageLoad } from './$types'

export const load: PageLoad = async ({ fetch, parent, params }) => {
  // The (app) layout rejects non-staff first; the API answers 404 to tickets the caller may not see.
  await parent()
  const api = createClient({ fetch })
  try {
    const [ticket, conversation, catalog] = await Promise.all([
      api.request('get', '/api/tickets/{id}', { path: { id: params.id } }),
      api.request('get', '/api/tickets/{id}/messages', { path: { id: params.id }, query: { limit: 100 } }),
      api.request('get', '/api/catalog'),
    ])
    // Staff who submitted the ticket themselves get the requester page, never staff handling.
    if (ticket.view !== 'staff') redirect(307, `/tickets/${params.id}`)
    return { ticket, conversation, catalog, unavailable: false as const }
  } catch (e) {
    if (e instanceof ApiError) {
      if (e.status === 0 || e.status >= 500) return { ticket: null, conversation: null, catalog: null, unavailable: true as const }
      error(e.status === 400 ? 404 : e.status, e.code)
    }
    throw e
  }
}
