import { fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, expect, test, vi } from 'vitest'
import Page from './+page.svelte'

const { invalidateAll } = vi.hoisted(() => ({ invalidateAll: vi.fn().mockResolvedValue(undefined) }))
vi.mock('$app/navigation', () => ({ invalidateAll }))
afterEach(() => { vi.unstubAllGlobals(); invalidateAll.mockClear() })

const base = {
  id: 'ticket-1', reference: 42, status: 'awaiting_confirmation', view: 'requester', subject: 'Printer', description: '<script>bad()</script>',
  category_name: 'Hardware', priority_name: 'Normal', intake_snapshot: null, ai_solution: null, resolution_source: null, closure_reason: null,
  proposal_version: 3, confirmation_deadline: '2026-12-01T10:00:00Z', requester: null,
  assignment: { team_name: 'Desk', assignee_display_name: 'Olga' }, created_at: '2026-10-01T10:00:00Z', updated_at: '2026-10-01T10:00:00Z',
  transitions: [{ id: 1, event_type: 'propose_resolution', to_status: 'awaiting_confirmation', created_at: '2026-10-01T10:00:00Z', payload: { summary: 'Replaced driver', proposal_version: 3 } }],
}
function show(over = {}, locale = 'en') {
  return render(Page, { props: { data: { ticket: { ...base, ...over }, locale, policy: { allow_reopening: false, requester_cancellation_policy: 'anytime_while_open' }, conversation: { items: [], next_cursor: null }, unavailable: false }, params: {} } as never })
}

test('shows the proposal summary/deadline and sends the exact proposal version', async () => {
  const fetch = vi.fn().mockResolvedValue(new Response('{"status":"closed"}'))
  vi.stubGlobal('fetch', fetch)
  show()
  expect(screen.getAllByText('Replaced driver').length).toBe(2)
  expect(screen.getByText('Automatic closure deadline:')).toBeTruthy()
  expect(screen.getByText('Olga')).toBeTruthy()
  await fireEvent.click(screen.getByRole('button', { name: 'Confirm resolved' }))
  await waitFor(() => expect(invalidateAll).toHaveBeenCalled())
  expect(JSON.parse(fetch.mock.calls[0][1].body)).toEqual({ action: 'accept_resolution', proposal_version: 3 })
})

test('cancellation validation and policy conflicts retain the reason without claiming success', async () => {
  const fetch = vi.fn()
    .mockResolvedValueOnce(new Response(JSON.stringify({ error: { code: 'validation_failed', details: { fields: { reason: ['required'] } } } }), { status: 422 }))
    .mockResolvedValueOnce(new Response(JSON.stringify({ error: { code: 'forbidden' } }), { status: 403 }))
  vi.stubGlobal('fetch', fetch)
  show()
  await fireEvent.click(screen.getByRole('button', { name: 'Cancel ticket' }))
  await fireEvent.click(screen.getByRole('button', { name: 'Confirm cancellation' }))
  await waitFor(() => expect(screen.getByLabelText('Cancellation reason').getAttribute('aria-invalid')).toBe('true'))
  await fireEvent.input(screen.getByLabelText('Cancellation reason'), { target: { value: 'No longer needed' } })
  await fireEvent.click(screen.getByRole('button', { name: 'Confirm cancellation' }))
  await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('The ticket or organization policy changed.'))
  expect((screen.getByLabelText('Cancellation reason') as HTMLTextAreaElement).value).toBe('No longer needed')
  expect(screen.queryByText('Ticket updated.')).toBeNull()
})

test('an uncertain reply send retries with the same key and keeps input', async () => {
  const fetch = vi.fn().mockRejectedValueOnce(new TypeError('lost response')).mockResolvedValueOnce(new Response('{}'))
  vi.stubGlobal('fetch', fetch)
  show({ status: 'queued' })
  await fireEvent.input(screen.getByLabelText('Message'), { target: { value: 'Still jammed.' } })
  await fireEvent.click(screen.getByRole('button', { name: 'Send reply' }))
  await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy())
  expect((screen.getByLabelText('Message') as HTMLTextAreaElement).value).toBe('Still jammed.')
  await fireEvent.click(screen.getByRole('button', { name: 'Send reply' }))
  await waitFor(() => expect(screen.getByText('Reply sent.')).toBeTruthy())
  expect(JSON.parse(fetch.mock.calls[0][1].body)).toEqual(JSON.parse(fetch.mock.calls[1][1].body))
})

test('AI-solved history escapes the suggestion and offers no mutation controls in German', () => {
  show({ status: 'ai_solved', ai_solution: '<img src=x onerror=bad()>', closure_reason: 'requester_confirmed', assignment: { team_name: null, assignee_display_name: null } }, 'de')
  expect(screen.getByText('<img src=x onerror=bad()>')).toBeTruthy()
  expect(document.querySelector('script, img')).toBeNull()
  expect(screen.queryByRole('textbox')).toBeNull()
  expect(screen.queryByRole('button', { name: /Antwort|wiedereröffnen|stornieren|bestätigen/i })).toBeNull()
  expect(screen.getByText('Nicht an den Support übergeben')).toBeTruthy()
})
