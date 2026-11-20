import { createClient, type Schemas } from '@tikkit/api-client'
import { reportFilters, reportQuery } from '#lib/reports.ts'
import type { PageServerLoad } from './$types'

export const load: PageServerLoad = async ({ fetch, parent, url }) => {
  await parent()
  const { request } = createClient({ fetch })
  const filters = reportFilters(url.searchParams, new Date())

  async function teamOptions() {
    const teams: Pick<Schemas['Team'], 'id' | 'name' | 'archived'>[] = []
    let cursor: string | undefined
    do {
      const page = await request('get', '/api/teams', { query: { limit: 100, cursor } })
      for (const { id, name, archived } of page.items) teams.push({ id, name, archived })
      cursor = page.next_cursor ?? undefined
    } while (cursor)
    return teams
  }

  const [catalog, teams, report] = await Promise.all([
    request('get', '/api/catalog/all'),
    teamOptions(),
    request('get', '/api/admin/reports', { query: reportQuery(filters) }),
  ])
  return { filters, categories: catalog.categories, teams, report }
}
