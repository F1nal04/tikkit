import { ApiError, createClient } from '@tikkit/api-client'
import { error } from '@sveltejs/kit'
import type { PageLoad } from './$types'

// Read-only: one GET, no change marker, no polling, no action endpoint.
export const load: PageLoad = async ({ fetch, parent, params }) => {
  await parent()
  try {
    return { submission: await createClient({ fetch }).request('get', '/api/admin/submissions/{id}', { path: { id: params.id } }) }
  } catch (e) {
    if (e instanceof ApiError && e.status >= 400 && e.status < 500) error(e.status === 400 ? 404 : e.status, e.code)
    throw e
  }
}
