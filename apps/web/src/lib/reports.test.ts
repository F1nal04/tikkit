import { expect, test } from 'vitest'
import { reportFilters, reportQuery } from './reports.ts'
import { submissionFilters, submissionQuery } from './submissions.ts'

const now = new Date('2026-11-01T00:15:00Z')

test('the default report includes 30 UTC calendar days', () => {
  const filters = reportFilters(new URLSearchParams(), now)
  expect(reportQuery(filters)).toMatchObject({
    created_from: '2026-10-03T00:00:00Z',
    created_before: '2026-11-02T00:00:00.000Z',
  })
})

test('report requests always have both bounds and a maximum of 366 UTC days', () => {
  const query = (params: string) => reportQuery(reportFilters(new URLSearchParams(params), now))
  expect(query('from=&until=2026-12-31')).toMatchObject({
    created_from: '2026-12-31T00:00:00Z',
    created_before: '2027-01-01T00:00:00.000Z',
  })
  expect(query('from=2024-01-01&until=2024-12-31').created_from).toBe('2024-01-01T00:00:00Z')
  expect(query('from=2023-01-01&until=2024-12-31').created_from).toBe('2024-01-01T00:00:00Z')
})

test('report drill-down scope survives submission filtering and paging', () => {
  const team = '50000000-0000-4000-8000-000000000001'
  const assignee = '00000000-0000-4000-8000-000000000002'
  const q = new URLSearchParams({
    team_id: team, assignee_id: assignee, entered_queue: 'true',
    ai_disposition: 'provider_failed', from: '2026-10-01', until: '2026-10-31',
  })
  expect(submissionQuery(submissionFilters(q))).toMatchObject({
    ...reportQuery(reportFilters(q, now)),
    ai_disposition: 'provider_failed',
    assignee_id: assignee,
    entered_queue: true,
  })
})
