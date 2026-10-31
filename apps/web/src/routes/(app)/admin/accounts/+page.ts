import { createClient } from '@tikkit/api-client'
import { allPages } from '#lib/admin.ts'
import type { PageLoad } from './$types'
export const load: PageLoad = async ({ fetch, parent, url }) => {
  await parent()
  const api = createClient({ fetch })
  const [accounts, invitations, teams] = await Promise.all([
    api.request('get', '/api/accounts', { query: { cursor: url.searchParams.get('accounts') ?? undefined } }),
    api.request('get', '/api/invitations', { query: { include_history: true, cursor: url.searchParams.get('invitations') ?? undefined } }),
    allPages((cursor) => api.request('get', '/api/teams', { query: { limit: 100, cursor } })),
  ])
  return { accounts: accounts.items, accountCursor: accounts.next_cursor, invitations: invitations.items, invitationCursor: invitations.next_cursor, teams }
}
