import { expect, test } from 'vitest'
import { canExport, reportFilters, reportQuery } from './reports.ts'
import { submissionFilters, submissionQuery } from './submissions.ts'

const now = new Date('2026-11-01T00:15:00Z')

test('the default report includes 30 UTC calendar days', () => {
  const filters = reportFilters(new URLSearchParams(), now)
  expect(reportQuery(filters)).toMatchObject({
    created_from: '2026-10-03T00:00:00Z',
    created_before: '2026-11-02T00:00:00.000Z',
  })
  expect(canExport(filters)).toBe(true)
})

test('CSV affordance requires both dates and allows at most 366 UTC days', () => {
  const allows = (query: string) => canExport(reportFilters(new URLSearchParams(query), now))
  expect(allows('from=&until=2026-12-31')).toBe(false)
  expect(allows('from=2024-01-01&until=2024-12-31')).toBe(true)
  expect(allows('from=2024-01-01&until=2025-01-01')).toBe(false)
})

test('report drill-down scope survives submission filtering and paging', () => {
  const team = '50000000-0000-4000-8000-000000000001'
  const q = new URLSearchParams({ team_id: team, ai_disposition: 'provider_failed', from: '2026-10-01', until: '2026-10-31' })
  expect(submissionQuery(submissionFilters(q))).toMatchObject({
    ...reportQuery(reportFilters(q, now)),
    ai_disposition: 'provider_failed',
  })
})
