import { ApiError, createClient } from '@tikkit/api-client'
import type { PageLoad } from './$types'

// Looking an invitation up never consumes it.
export const load: PageLoad = async ({ fetch, params }) => {
  try {
    const invitation = await createClient({ fetch }).request('post', '/api/invitations/lookup', {
      body: { token: params.token },
    })
    return { invitation, lookupError: null }
  } catch (e) {
    return { invitation: null, lookupError: e instanceof ApiError ? e.code : 'unavailable' }
  }
}
