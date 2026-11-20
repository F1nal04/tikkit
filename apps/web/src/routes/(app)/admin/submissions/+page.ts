import { createClient } from '@tikkit/api-client'
import { submissionFilters, submissionQuery } from '#lib/submissions.ts'
import type { PageLoad } from './$types'

export const load: PageLoad = async ({ fetch, parent, url }) => {
  await parent()
  const { request } = createClient({ fetch })
  const filters = submissionFilters(url.searchParams)
  const [catalog, submissions] = await Promise.all([
    request('get', '/api/catalog/all'),
    request('get', '/api/admin/submissions', { query: submissionQuery(filters) }),
  ])
  return { filters, categories: catalog.categories, submissions }
}
