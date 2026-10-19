import { createClient } from '@tikkit/api-client'
import { allPages } from '#lib/admin.ts'
import type { PageLoad } from './$types'
export const load: PageLoad = async ({ fetch, parent, url }) => {
  await parent()
  const api = createClient({ fetch })
  const [teams, accounts] = await Promise.all([
    api.request('get', '/api/teams', { query: { cursor: url.searchParams.get('cursor') ?? undefined } }),
    allPages((cursor) => api.request('get', '/api/accounts', { query: { limit: 100, cursor } })),
  ])
  return { teams: teams.items, nextCursor: teams.next_cursor, accounts }
}
