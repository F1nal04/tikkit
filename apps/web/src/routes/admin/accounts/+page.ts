import { error } from '@sveltejs/kit'
import { createClient } from '@tikkit/api-client'
import type { PageLoad } from './$types'

// ponytail: first 100 of each list; add paging controls when deployments outgrow it.
export const load: PageLoad = async ({ fetch, parent }) => {
  const { user } = await parent()
  // Navigation aid only; the API answers 403 to non-Admins.
  if (user?.role !== 'admin') error(403, 'forbidden')
  const api = createClient({ fetch })
  const [accounts, invitations] = await Promise.all([
    api.request('get', '/api/accounts', { query: { limit: 100 } }),
    api.request('get', '/api/invitations', { query: { limit: 100 } }),
  ])
  return { accounts: accounts.items, invitations: invitations.items }
}
