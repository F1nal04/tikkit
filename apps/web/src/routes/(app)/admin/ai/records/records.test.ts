import { render, screen, within } from '@testing-library/svelte'
import { expect, test } from 'vitest'
import Page from './+page.svelte'

const stats = {
  days: 30, solved: 4, not_solved: 3, skipped: 2, failed: 1, category_exempt: 5, cancelled: 6, processing: 7, awaiting_decision: 8,
  failures: [{ outcome: 'ai_disabled', count: 1 }, { outcome: 'budget_org_requests', count: 2 }, { outcome: 'brand_new_code', count: 3 }],
}
const record = (over: object = {}) => ({
  id: 'attempt-1', ticket_id: 'ticket-1', reference: 42, subject: 'Printer <b>jams</b>', status: 'ai_solved',
  requester: { id: 'r1', username: 'rita', display_name: 'Rita Requester' }, outcome: 'solved', attempt_state: 'suggestion_ready',
  outcome_code: null, provider: 'openai', model: 'model-a', created_at: '2026-10-01T10:00:00Z', decided_at: '2026-10-01T10:05:00Z', ...over,
})
const outcomes = ['solved', 'not_solved', 'skipped', 'failed', 'cancelled', 'awaiting_decision', 'processing']
function show(over: object = {}, locale = 'en') {
  const data = { locale, stats, records: { items: [record()], next_cursor: null }, outcome: '', cursor: '', outcomes, ...over }
  return render(Page, { props: { data, params: {} } as never })
}

test('shows outcome counts and failure reasons with translated codes', () => {
  show()
  const counts = within(screen.getByTestId('ai-outcomes'))
  expect(counts.getByText('Solved by AI, confirmed by the requester').nextElementSibling?.textContent).toBe('4')
  expect(counts.getByText('Sent to the help desk after a suggestion').nextElementSibling?.textContent).toBe('3')
  expect(counts.getByText('AI skipped by the requester').nextElementSibling?.textContent).toBe('2')
  expect(counts.getByText('Sent to the help desk without a suggestion').nextElementSibling?.textContent).toBe('1')
  const open = within(screen.getByTestId('ai-open'))
  expect(open.getByText('Waiting for the requester to decide').nextElementSibling?.textContent).toBe('8')
  expect(screen.getByText('AI assistance was switched off')).toBeTruthy()
  expect(screen.getByText('Request budget of the organization used up')).toBeTruthy()
  // An unknown code is shown as it is rather than hidden.
  expect(screen.getByText('brand_new_code')).toBeTruthy()
})

test('lists records with a link to the read-only ticket and escapes ticket text', () => {
  show({ records: { items: [record(), record({ id: 'attempt-2', ticket_id: 'ticket-2', reference: 43, status: 'queued', outcome: 'failed', outcome_code: 'timeout', decided_at: null })], next_cursor: null } })
  const link = screen.getByRole('link', { name: '#42 Printer <b>jams</b>' })
  expect(link.getAttribute('href')).toBe('/queue/ticket-1')
  expect(document.querySelector('td b, th b')).toBeNull()
  const solved = document.querySelector('tr[data-outcome="solved"]') as HTMLElement
  expect(within(solved).getAllByText('Solved by AI').length).toBe(2) // outcome and status badge
  expect(within(solved).getByText('Rita Requester (rita)')).toBeTruthy()
  expect(within(solved).getByText('OpenAI · model-a')).toBeTruthy()
  const failed = document.querySelector('tr[data-outcome="failed"]') as HTMLElement
  expect(within(failed).getByText('Took too long')).toBeTruthy()
  expect(failed.querySelectorAll('time').length).toBe(1)
  // Records are read-only: the page has no control that changes a ticket.
  expect(screen.getAllByRole('button').map((b) => b.textContent)).toEqual(['Apply'])
})

test('filter and pager links keep the selected outcome', () => {
  show({ outcome: 'not_solved', cursor: 'abc', records: { items: [record({ outcome: 'not_solved', status: 'queued' })], next_cursor: 'next-1' } })
  expect((screen.getByLabelText('Outcome') as HTMLSelectElement).value).toBe('not_solved')
  expect(screen.getByRole('link', { name: 'First page' }).getAttribute('href')).toBe('/admin/ai/records?outcome=not_solved')
  expect(screen.getByRole('link', { name: 'Next page' }).getAttribute('href')).toBe('/admin/ai/records?outcome=not_solved&cursor=next-1')
})

test('German labels and the empty state', () => {
  show({ records: { items: [], next_cursor: null } }, 'de')
  expect(screen.getByRole('heading', { level: 1, name: 'KI-Ergebnisse' })).toBeTruthy()
  expect(screen.getByText('Keine Tickets mit diesem Ergebnis.')).toBeTruthy()
  expect(screen.getByText('Von der KI gelöst, vom Antragsteller bestätigt')).toBeTruthy()
  expect(screen.queryByRole('navigation', { name: 'Seiten der Vorgänge' })).toBeNull()
})
