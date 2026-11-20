import { createClient } from '@tikkit/api-client'
import { auditFilters, auditQuery } from '#lib/audit.ts'
import type { PageLoad } from './$types'

export const load: PageLoad = async ({ fetch, parent, url }) => {
  await parent()
  const { request } = createClient({ fetch })
  const filters = auditFilters(url.searchParams)
  const [known, events] = await Promise.all([request('get', '/api/admin/audit/actions'), request('get', '/api/admin/audit', { query: auditQuery(filters) })])
  return { filters, actions: known.actions, events }
}
