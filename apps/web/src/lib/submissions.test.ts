import { expect, test } from 'vitest'
import { inHelpDesk, isAiResolution, submissionFilters, submissionQuery, submissionsHref } from './submissions.ts'

const parse = (query: string) => submissionFilters(new URLSearchParams(query))
const CATEGORY = '10000000-0000-4000-8000-000000000001'

test('filters come from the address bar; unknown values fall back to all', () => {
  expect(parse('')).toEqual({ category_id: '', status: '', resolution_source: '', ai_outcome: '', from: '', until: '', cursor: '' })
  expect(parse(`category_id=${CATEGORY.toUpperCase()}&status=ai_solved&resolution_source=ai&ai_outcome=solved&from=2026-10-01&until=2026-10-31&cursor=abc`)).toEqual({
    category_id: CATEGORY, status: 'ai_solved', resolution_source: 'ai', ai_outcome: 'solved', from: '2026-10-01', until: '2026-10-31', cursor: 'abc',
  })
  expect(parse('category_id=nope&status=draft&resolution_source=human&ai_outcome=reopened&from=2026-02-30&until=tomorrow')).toEqual(parse(''))
  // A reversed range is swapped rather than sent as an empty one.
  expect(parse('from=2026-10-31&until=2026-10-01')).toMatchObject({ from: '2026-10-01', until: '2026-10-31' })
  expect(parse('ai_outcome=none&resolution_source=none')).toMatchObject({ ai_outcome: 'none', resolution_source: 'none' })
})

test('the API query uses inclusive UTC days and the page size', () => {
  expect(submissionQuery(parse('from=2026-10-01&until=2026-10-31&ai_outcome=awaiting_decision'))).toEqual({
    category_id: undefined, status: undefined, resolution_source: undefined, ai_outcome: 'awaiting_decision',
    created_from: '2026-10-01T00:00:00Z', created_before: '2026-11-01T00:00:00.000Z', cursor: undefined, limit: 25,
  })
})

test('links keep the filters and drop empty ones', () => {
  expect(submissionsHref({})).toBe('/admin/submissions')
  expect(submissionsHref({ ...parse('status=closed&cursor=c1'), cursor: '' })).toBe('/admin/submissions?status=closed')
  expect(submissionsHref({ ai_outcome: 'failed', cursor: 'n' })).toBe('/admin/submissions?ai_outcome=failed&cursor=n')
})

test('only a confirmed AI outcome is an AI resolution; AI-stage records are not help desk work', () => {
  expect(isAiResolution('solved')).toBe(true)
  for (const outcome of ['awaiting_decision', 'processing', 'cancelled', 'failed', 'skipped', 'not_solved', null] as const) expect(isAiResolution(outcome)).toBe(false)
  expect(inHelpDesk({ status: 'ai_solved', ai_outcome: 'solved' })).toBe(false)
  expect(inHelpDesk({ status: 'awaiting_ai_decision', ai_outcome: 'awaiting_decision' })).toBe(false)
  expect(inHelpDesk({ status: 'queued', ai_outcome: null })).toBe(true)
  expect(inHelpDesk({ status: 'closed', ai_outcome: 'not_solved' })).toBe(true)
  // Cancelled before any handoff: never help desk work. Cancelled after a failed attempt: it was.
  expect(inHelpDesk({ status: 'cancelled', ai_outcome: 'cancelled' })).toBe(false)
  expect(inHelpDesk({ status: 'cancelled', ai_outcome: 'failed' })).toBe(true)
  expect(inHelpDesk({ status: 'cancelled', ai_outcome: null })).toBe(true)
})
