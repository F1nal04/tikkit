import { createClient } from '@tikkit/api-client'
import type { PageLoad } from './$types'

// ponytail: the first 20 drafts; older ones stay saved and are reachable once someone discards newer ones.
export const load: PageLoad = async ({ fetch, parent, depends }) => {
  await parent()
  depends('tikkit:catalog')
  const api = createClient({ fetch })
  const [catalog, drafts, aiPolicy] = await Promise.all([
    api.request('get', '/api/catalog'),
    api.request('get', '/api/ticket-drafts', { query: { limit: 20 } }),
    // Only what to disclose and whether to offer a skip; Rust decides the route at submission.
    api.request('get', '/api/ai/policy'),
  ])
  return { catalog, drafts: drafts.items, aiPolicy }
}
