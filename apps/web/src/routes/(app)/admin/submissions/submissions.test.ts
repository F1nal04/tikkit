import { render, screen, within } from '@testing-library/svelte'
import { expect, test } from 'vitest'
import { submissionFilters } from '#lib/submissions.ts'
import Page from './+page.svelte'

const CATEGORY = '10000000-0000-4000-8000-000000000001'
const item = (over: object = {}) => ({
  id: 'ticket-1', reference: 42, subject: 'Printer <b>jams</b>', status: 'ai_solved', category_id: CATEGORY, category_name: 'Hardware', priority_name: 'Normal',
  requester: { id: 'r1', username: 'rita', display_name: 'Rita Requester' }, team_name: null, assignee_display_name: null,
  resolution_source: 'ai', closure_reason: 'requester_confirmed', resolution: 'ai_confirmed', route: 'ai', ai_outcome: 'solved',
  created_at: '2026-10-01T10:00:00Z', updated_at: '2026-10-01T10:05:00Z', ...over,
})
const categories = [{ id: CATEGORY, name: 'Hardware', skip_ai: false, archived: false }, { id: 'c2', name: 'Old', skip_ai: false, archived: true }]
function show(over: { items?: object[]; next_cursor?: string | null; query?: string } = {}, locale = 'en') {
  const data = { locale, filters: submissionFilters(new URLSearchParams(over.query ?? '')), categories, submissions: { items: over.items ?? [item()], next_cursor: over.next_cursor ?? null } }
  return render(Page, { props: { data, params: {} } as never })
}

test('lists mixed outcomes and tells unconfirmed and abandoned attempts apart from solved ones', () => {
  show({ items: [
    item(),
    item({ id: 't2', reference: 43, status: 'awaiting_ai_decision', resolution: 'open', resolution_source: null, closure_reason: null, ai_outcome: 'awaiting_decision' }),
    item({ id: 't3', reference: 44, status: 'cancelled', resolution: 'cancelled', resolution_source: null, closure_reason: null, ai_outcome: 'cancelled' }),
    item({ id: 't4', reference: 45, status: 'queued', resolution: 'open', resolution_source: null, closure_reason: null, ai_outcome: 'failed' }),
    item({ id: 't5', reference: 46, status: 'closed', resolution: 'confirmation_timeout', resolution_source: 'operator', closure_reason: 'confirmation_timeout', ai_outcome: null, route: 'ai_disabled' }),
  ] })
  const row = (outcome: string) => document.querySelector(`tr[data-ai-outcome="${outcome}"]`) as HTMLElement
  expect(within(row('solved')).getByText('Solved by AI: confirmed by the requester')).toBeTruthy()
  expect(within(row('solved')).getByText('Solved by AI, confirmed by the requester')).toBeTruthy()
  expect(within(row('awaiting_decision')).getByText('Unconfirmed: suggestion shown, no decision')).toBeTruthy()
  expect(within(row('awaiting_decision')).getByText('Not resolved yet')).toBeTruthy()
  expect(within(row('cancelled')).getByText('Abandoned: cancelled without a decision')).toBeTruthy()
  expect(within(row('failed')).getByText('Failed: no suggestion, sent to the help desk')).toBeTruthy()
  expect(within(row('none')).getByText('No AI attempt')).toBeTruthy()
  expect(within(row('none')).getByText('Resolved by the help desk, closed automatically without a confirmation')).toBeTruthy()
  // Ticket text is escaped and links to the read-only detail.
  expect(screen.getByRole('link', { name: '#42 Printer <b>jams</b>' }).getAttribute('href')).toBe('/admin/submissions/ticket-1')
  expect(document.querySelector('th b, td b')).toBeNull()
  // Read-only: the only button applies the filters.
  expect(screen.getAllByRole('button').map((b) => b.textContent)).toEqual(['Apply filters'])
})

test('filters show the current values, archived categories, and keep them in the pager', () => {
  show({ query: `ai_outcome=none&status=closed&category_id=${CATEGORY}&from=2026-10-01&cursor=abc`, next_cursor: 'next-1' })
  expect((screen.getByLabelText('AI outcome') as HTMLSelectElement).value).toBe('none')
  expect((screen.getByLabelText('Status') as HTMLSelectElement).value).toBe('closed')
  expect((screen.getByLabelText('Category') as HTMLSelectElement).value).toBe(CATEGORY)
  expect((screen.getByLabelText('Submitted from (UTC)') as HTMLInputElement).value).toBe('2026-10-01')
  expect(screen.getByRole('option', { name: 'Old (archived)' })).toBeTruthy()
  expect(screen.getByRole('link', { name: 'First page' }).getAttribute('href')).toBe(`/admin/submissions?category_id=${CATEGORY}&status=closed&ai_outcome=none&from=2026-10-01`)
  expect(screen.getByRole('link', { name: 'Next page' }).getAttribute('href')).toBe(`/admin/submissions?category_id=${CATEGORY}&status=closed&ai_outcome=none&from=2026-10-01&cursor=next-1`)
})

test('German labels and the empty state', () => {
  show({ items: [] }, 'de')
  expect(screen.getByRole('heading', { level: 1, name: 'Einreichungen' })).toBeTruthy()
  expect(screen.getByText('Keine Einreichungen passen zu diesen Filtern.')).toBeTruthy()
  expect(screen.getByRole('option', { name: 'Abgebrochen: ohne Entscheidung storniert' })).toBeTruthy()
  expect(screen.queryByRole('navigation', { name: 'Seiten der Einreichungen' })).toBeNull()
})
