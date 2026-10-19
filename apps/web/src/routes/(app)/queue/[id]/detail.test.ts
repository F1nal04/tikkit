import { fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, expect, test, vi } from 'vitest'
import Page from './+page.svelte'

const { invalidateAll } = vi.hoisted(() => ({ invalidateAll: vi.fn().mockResolvedValue(undefined) }))
vi.mock('$app/navigation', () => ({ invalidateAll }))
afterEach(() => { vi.unstubAllGlobals(); invalidateAll.mockClear() })

const base = {
  id: 'ticket-1', reference: 42, status: 'in_progress', view: 'staff', subject: 'Printer', description: '<b>Jams</b>',
  category_name: 'Hardware', priority_id: 'p-normal', priority_name: 'Normal', intake_snapshot: null,
  resolution_source: null, closure_reason: null, proposal_version: 0, confirmation_deadline: null,
  requester: { id: 'r1', username: 'requester', display_name: 'Rita Requester' },
  assignment: { team_name: 'Desk', assignee_display_name: 'Olga' }, created_at: '2026-10-01T10:00:00Z', updated_at: '2026-10-02T10:00:00.123456Z',
  transitions: [], priority_changes: [], staff_access: { can_handle: true, can_reopen: false },
}
const catalog = { priorities: [{ id: 'p-normal', name: 'Normal' }, { id: 'p-urgent', name: 'Urgent' }] }
function show(over = {}, locale = 'en', items: unknown[] = []) {
  return render(Page, { props: { data: { ticket: { ...base, ...over }, locale, catalog, conversation: { items, next_cursor: null }, unavailable: false }, params: {} } as never })
}
const bodyOf = (fetch: ReturnType<typeof vi.fn>, call = 0) => JSON.parse(fetch.mock.calls[call][1].body)

test('shows intake, requester identity, and escapes user text', () => {
  show()
  expect(screen.getByText('Rita Requester')).toBeTruthy()
  expect(screen.getByText('<b>Jams</b>')).toBeTruthy()
  expect(document.querySelector('b')).toBeNull()
  expect(screen.getByRole('button', { name: 'Wait for requester' })).toBeTruthy()
})

test('another member of a claimed ticket gets a read-only page without any write control', () => {
  show({ staff_access: { can_handle: false, can_reopen: false } })
  expect(screen.getByText(/assigned to someone else/)).toBeTruthy()
  expect(screen.queryByRole('textbox')).toBeNull()
  expect(screen.queryByRole('button', { name: /Send|Add internal|Propose|Save priority|Wait for|Start work|Reopen/ })).toBeNull()
  expect(screen.getByText('Olga')).toBeTruthy()
})

test('public reply and internal note are labelled before sending and post to their endpoints', async () => {
  const fetch = vi.fn().mockImplementation(async () => new Response('{}', { status: 201 }))
  vi.stubGlobal('fetch', fetch)
  show()
  expect(screen.getByText(/The requester will see this reply/)).toBeTruthy()
  await fireEvent.input(screen.getByLabelText('Message'), { target: { value: 'Try again' } })
  await fireEvent.click(screen.getByRole('button', { name: 'Send public reply' }))
  await waitFor(() => expect(screen.getByText('Public reply sent.')).toBeTruthy())
  expect(fetch.mock.calls[0][0].url ?? String(fetch.mock.calls[0][0])).toContain('/api/tickets/ticket-1/replies')

  await fireEvent.click(screen.getByLabelText('Internal note'))
  expect(screen.getByText(/never shown to the requester/)).toBeTruthy()
  await fireEvent.input(screen.getByLabelText('Message'), { target: { value: 'Looks like the roller' } })
  await fireEvent.click(screen.getByRole('button', { name: 'Add internal note' }))
  await waitFor(() => expect(screen.getByText('Internal note added.')).toBeTruthy())
  expect(String(fetch.mock.calls[1][0].url ?? fetch.mock.calls[1][0])).toContain('/api/tickets/ticket-1/notes')
  expect(bodyOf(fetch, 1).body).toBe('Looks like the roller')
})

test('notes in the conversation are marked staff-only; public replies visible to the requester', () => {
  const msg = (id: string, kind: string) => ({ id, kind, author_kind: 'staff', author_display_name: 'Olga', body: `body ${id}`, screenshot_ids: [], created_at: '2026-10-02T10:00:00Z' })
  show({}, 'en', [msg('a', 'public_reply'), msg('b', 'internal_note')])
  expect(screen.getByText(/Internal note · Only support staff can see this/)).toBeTruthy()
  expect(screen.getByText(/Public reply · Visible to the requester/)).toBeTruthy()
})

test('a stale proposal is a recoverable conflict: nothing is claimed saved and the summary is kept', async () => {
  const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: { code: 'conflict' } }), { status: 409 }))
  vi.stubGlobal('fetch', fetch)
  show()
  await fireEvent.input(screen.getByLabelText(/Resolution summary/), { target: { value: 'Replaced the roller' } })
  await fireEvent.click(screen.getByRole('button', { name: 'Propose resolution' }))
  await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('changed since you loaded it'))
  expect(bodyOf(fetch)).toEqual({ action: 'propose_resolution', summary: 'Replaced the roller', expected_updated_at: '2026-10-02T10:00:00.123456Z' })
  expect(invalidateAll).toHaveBeenCalled()
  expect((screen.getByLabelText(/Resolution summary/) as HTMLTextAreaElement).value).toBe('Replaced the roller')
  expect(screen.queryByText('Ticket updated.')).toBeNull()
})

test('awaiting confirmation says so, offers a replacement, and never a close button', () => {
  show({ status: 'awaiting_confirmation', proposal_version: 2, confirmation_deadline: '2026-12-01T10:00:00Z',
    transitions: [{ id: 1, event_type: 'propose_resolution', from_status: 'in_progress', to_status: 'awaiting_confirmation', actor_account_id: 'a', actor_display_name: 'Olga', payload: { summary: 'Swapped roller', proposal_version: 2 }, created_at: '2026-10-02T10:00:00Z' }] })
  expect(screen.getByText(/Awaiting requester confirmation. The ticket is not closed./)).toBeTruthy()
  expect(screen.getAllByText(/Swapped roller/).length).toBeGreaterThan(0)
  expect(screen.getByRole('button', { name: 'Replace proposal' })).toBeTruthy()
  expect(screen.queryByRole('button', { name: /close/i })).toBeNull()
})

test('reopen shows only when the API says it is allowed; priority needs a different level', async () => {
  const fetch = vi.fn().mockResolvedValue(new Response('{"status":"in_progress"}'))
  vi.stubGlobal('fetch', fetch)
  show({ status: 'closed', staff_access: { can_handle: false, can_reopen: true } })
  expect(screen.queryByText(/read this ticket/)).toBeNull()
  await fireEvent.click(screen.getByRole('button', { name: 'Reopen ticket' }))
  await waitFor(() => expect(bodyOf(fetch).action).toBe('reopen'))
})

test('priority choices exclude the current level and send the stale-write token', async () => {
  const fetch = vi.fn().mockResolvedValue(new Response('{"status":"in_progress"}'))
  vi.stubGlobal('fetch', fetch)
  show()
  const select = screen.getByLabelText('New priority') as HTMLSelectElement
  expect([...select.options].map((o) => o.text)).toEqual(['Choose a priority', 'Urgent'])
  expect((screen.getByRole('button', { name: 'Save priority' }) as HTMLButtonElement).disabled).toBe(true)
  await fireEvent.change(select, { target: { value: 'p-urgent' } })
  await fireEvent.click(screen.getByRole('button', { name: 'Save priority' }))
  await waitFor(() => expect(bodyOf(fetch)).toEqual({ action: 'set_priority', priority_id: 'p-urgent', expected_updated_at: '2026-10-02T10:00:00.123456Z' }))
})

test('German labels and history actors', () => {
  show({ priority_changes: [{ id: 1, previous_priority_name: 'Normal', priority_name: 'Urgent', actor_account_id: 'a', actor_display_name: 'Olga', created_at: '2026-10-02T10:00:00Z' }] }, 'de')
  expect(screen.getByRole('button', { name: 'Öffentliche Antwort senden' })).toBeTruthy()
  expect(screen.getByText('Prioritätsverlauf')).toBeTruthy()
  expect(screen.getByText(/Normal → Urgent/)).toBeTruthy()
})


test('AI handoff shows unavailable suggestion text without a stored solution field', () => {
  show({ transitions: [{ id: 1, event_type: 'ai_not_solved', from_status: 'awaiting_ai_decision', to_status: 'queued', payload: { route: 'not_solved' }, created_at: '2026-10-02T10:00:00Z' }] })
  expect(screen.getByText('The AI suggestion text is unavailable for this record.')).toBeTruthy()
})
