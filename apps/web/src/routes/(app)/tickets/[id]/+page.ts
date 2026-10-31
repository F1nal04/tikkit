import { ApiError, createClient } from '@tikkit/api-client'
import { error } from '@sveltejs/kit'
import type { PageLoad } from './$types'

export const load: PageLoad = async ({ fetch, parent, params }) => {
  await parent()
  const api = createClient({ fetch })
  try {
    const [ticket, conversation, policy] = await Promise.all([
      api.request('get', '/api/tickets/{id}', { path: { id: params.id } }),
      api.request('get', '/api/tickets/{id}/messages', { path: { id: params.id }, query: { limit: 25 } }),
      api.request('get', '/api/organization'),
    ])
    // This page serves own submissions. Staff detail is a separate workflow.
    if (ticket.view !== 'requester') error(404, 'not_found')
    return { ticket, conversation, policy, unavailable: false }
  } catch (e) {
    if (e instanceof ApiError) {
      if (e.status === 0 || e.status >= 500) return { ticket: null, conversation: null, policy: null, unavailable: true }
      error(e.status === 400 ? 404 : e.status, e.code)
    }
    throw e
  }
}
