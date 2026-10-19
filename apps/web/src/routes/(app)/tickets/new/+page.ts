import { createClient } from '@tikkit/api-client'
import type { PageLoad } from './$types'

// ponytail: the first 20 drafts; older ones stay saved and are reachable once someone discards newer ones.
export const load: PageLoad = async ({ fetch, parent, depends }) => {
  await parent()
  depends('tikkit:catalog')
  const api = createClient({ fetch })
  const [catalog, drafts] = await Promise.all([
    api.request('get', '/api/catalog'),
    api.request('get', '/api/ticket-drafts', { query: { limit: 20 } }),
  ])
  return { catalog, drafts: drafts.items }
}
