import { fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, expect, test, vi } from 'vitest'
import { AI_POLL_MS } from '#lib/aiStep.ts'
import AiStep from './AiStep.svelte'
import Page from './+page.svelte'

const { invalidateAll } = vi.hoisted(() => ({ invalidateAll: vi.fn().mockResolvedValue(undefined) }))
vi.mock('$app/navigation', () => ({ invalidateAll }))
afterEach(() => { vi.unstubAllGlobals(); vi.useRealTimers(); invalidateAll.mockClear() })

const suggestion = { explanation: 'Reset the <b>profile</b>.\n[link](https://evil.example)', steps: ['Close Word <img src=x onerror=alert(1)>', 'Open Word'] }
const attempt = (over = {}) => ({
  state: 'suggestion_ready', language: 'de', suggestion, reason: null,
  created_at: '2026-11-16T10:00:00Z', finished_at: '2026-11-16T10:00:12Z', decision: null, decided_at: null, ...over,
})
const base = {
  id: 'ticket-1', reference: 42, status: 'awaiting_ai_decision', view: 'requester', subject: 'Word', description: 'Crashes',
  category_name: 'Software', priority_name: 'Normal', intake_snapshot: null, resolution_source: null, closure_reason: null,
  proposal_version: 0, confirmation_deadline: null, requester: null, ai: attempt(),
  assignment: { team_name: null, assignee_display_name: null }, created_at: '2026-11-16T10:00:00Z', updated_at: '2026-11-16T10:00:12Z',
  transitions: [],
}
const policy = (skip = false) => ({ ai_assistance: true, requester_skip_allowed: skip })
const step = (over = {}, props = {}) =>
  render(AiStep, { props: { ticket: { ...base, ...over }, policy: policy(), locale: 'en', ...props } as never })
const error = (status: number, code: string, details?: object) =>
  new Response(JSON.stringify({ error: { code, message: code, details }, request_id: 'r' }), { status })
const posted = (fetch: ReturnType<typeof vi.fn>) => fetch.mock.calls.filter(([, init]) => init?.method === 'POST')

test('the suggestion is labelled as AI-generated, shown as plain text in its own language, with exactly two outcomes', () => {
  const fetch = vi.fn()
  vi.stubGlobal('fetch', fetch)
  step()
  expect(screen.getByRole('heading', { name: 'AI-generated suggestion' })).toBeTruthy()
  expect(screen.getByText(/may be incomplete or wrong/)).toBeTruthy()
  const box = screen.getByTestId('ai-suggestion')
  expect(box.getAttribute('lang')).toBe('de')
  expect(box.textContent).toContain('Reset the <b>profile</b>.\n[link](https://evil.example)')
  expect(box.querySelectorAll('li')).toHaveLength(2)
  expect(box.querySelector('b, a, img, script')).toBeNull()
  // Shown as generated: the German text under an English interface is not translated.
  expect(screen.getByText(/shown exactly as it was generated, in Deutsch/)).toBeTruthy()
  expect(screen.getAllByRole('button').map((b) => b.textContent)).toEqual(['Solved', 'Still need help'])
  expect(screen.queryByRole('textbox')).toBeNull()
  // Rendering and reading the suggestion requests nothing.
  expect(fetch).not.toHaveBeenCalled()
})

test('only the Solved click requests the AI resolution, and a second click while it runs is ignored', async () => {
  let answer!: (r: Response) => void
  const fetch = vi.fn().mockReturnValue(new Promise<Response>((resolve) => (answer = resolve)))
  vi.stubGlobal('fetch', fetch)
  step()
  const solved = screen.getByRole('button', { name: 'Solved' })
  await fireEvent.click(solved)
  await fireEvent.click(solved)
  await fireEvent.click(screen.getByRole('button', { name: 'Still need help' }))
  expect(fetch).toHaveBeenCalledTimes(1)
  expect(fetch.mock.calls[0][0]).toBe('/api/tickets/ticket-1/actions')
  expect(JSON.parse(fetch.mock.calls[0][1].body)).toEqual({ action: 'ai_solved' })
  expect((solved as HTMLButtonElement).disabled).toBe(true)
  answer(new Response('{"status":"ai_solved"}'))
  await waitFor(() => expect(invalidateAll).toHaveBeenCalledTimes(1))
  expect(screen.queryByRole('alert')).toBeNull()
})

test('a lost race reloads and explains the committed state without claiming success', async () => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(error(409, 'conflict', { fields: {}, status: 'queued' })))
  step()
  await fireEvent.click(screen.getByRole('button', { name: 'Solved' }))
  await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('already decided or changed somewhere else'))
  expect(screen.getByRole('alert').textContent).toContain('current state: Queued.')
  expect(invalidateAll).toHaveBeenCalledTimes(1)
})

test('an uncertain response keeps both actions for a safe retry', async () => {
  const fetch = vi.fn().mockRejectedValueOnce(new TypeError('lost')).mockResolvedValueOnce(new Response('{"status":"queued"}'))
  vi.stubGlobal('fetch', fetch)
  step()
  await fireEvent.click(screen.getByRole('button', { name: 'Still need help' }))
  await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('We could not confirm that your decision was saved.'))
  expect(invalidateAll).not.toHaveBeenCalled()
  await fireEvent.click(screen.getByRole('button', { name: 'Still need help' }))
  await waitFor(() => expect(invalidateAll).toHaveBeenCalledTimes(1))
  expect(screen.queryByRole('alert')).toBeNull()
  expect(posted(fetch).map(([, init]) => JSON.parse(init.body))).toEqual([{ action: 'ai_not_solved' }, { action: 'ai_not_solved' }])
})

test('waiting: no bypass by default, the skip only under the Admin policy, and a refused skip is explained', async () => {
  const fetch = vi.fn().mockResolvedValue(error(403, 'forbidden'))
  vi.stubGlobal('fetch', fetch)
  const waiting = { status: 'ai_processing', ai: attempt({ state: 'processing', suggestion: null, finished_at: null }) }
  const view = step(waiting)
  expect(screen.getByRole('heading', { name: 'Preparing an AI suggestion' })).toBeTruthy()
  expect(screen.getByText(/analyzing your description, the program and other details you entered, and any screenshots you attached/)).toBeTruthy()
  expect(screen.queryByRole('button')).toBeNull()
  expect(screen.getByText('Your organization requires this step before a request goes to the support team.')).toBeTruthy()

  await view.rerender({ policy: policy(true) })
  await fireEvent.click(screen.getByRole('button', { name: 'Skip the AI suggestion and send to support' }))
  await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('Skipping the AI suggestion is not allowed by your organization.'))
  expect(JSON.parse(fetch.mock.calls[0][1].body)).toEqual({ action: 'skip_ai' })
  expect(invalidateAll).toHaveBeenCalledTimes(1)
})

test('the change from waiting to the suggestion is announced and focused; a later handoff too', async () => {
  vi.stubGlobal('fetch', vi.fn())
  const view = step({ status: 'ai_processing', ai: attempt({ state: 'processing', suggestion: null }) })
  expect(screen.getByTestId('ai-announcement').textContent).toBe('')
  await view.rerender({ ticket: base } as never)
  await waitFor(() => expect(document.activeElement).toBe(screen.getByRole('heading', { name: 'AI-generated suggestion' })))
  expect(screen.getByTestId('ai-announcement').textContent).toContain('The AI suggestion is ready.')
  expect(screen.getByTestId('ai-announcement').getAttribute('role')).toBe('status')
  await view.rerender({ ticket: { ...base, status: 'queued', ai: attempt({ decision: 'not_solved', decided_at: '2026-11-16T10:05:00Z' }) } } as never)
  await waitFor(() => expect(document.activeElement).toBe(screen.getByRole('heading', { name: 'Sent to the support team' })))
  expect(screen.getByTestId('ai-announcement').textContent).toBe('Your request was sent to the support team.')
  expect(screen.queryByRole('button')).toBeNull()
})

test('receipts in German: each handoff reason, and the final AI-solved record', async () => {
  vi.stubGlobal('fetch', vi.fn())
  const reasons = {
    timeout: /hat zu lange gedauert/, ai_failed: /Es konnte kein KI-Vorschlag erstellt werden/,
    ai_unavailable: /KI-Unterstützung war nicht verfügbar/, requester_skip: /Du hast den KI-Vorschlag übersprungen/,
  }
  for (const [reason, text] of Object.entries(reasons)) {
    const view = step({ status: 'queued', ai: attempt({ state: 'handed_off', suggestion: null, reason }) }, { locale: 'de' })
    expect(screen.getByRole('heading', { name: 'An das Support-Team gesendet' })).toBeTruthy()
    expect(screen.getByText(text)).toBeTruthy()
    expect(screen.getByText(/Deine ursprüngliche Anfrage #42 wurde unverändert weitergegeben/)).toBeTruthy()
    expect(screen.queryByRole('button')).toBeNull()
    view.unmount()
  }
  step({ status: 'ai_solved', closure_reason: 'requester_confirmed', ai: attempt({ decision: 'solved', decided_at: '2026-11-16T10:05:00Z' }) }, { locale: 'de' })
  expect(screen.getByRole('heading', { name: 'Mit dem KI-Vorschlag gelöst' })).toBeTruthy()
  expect(screen.getByText(/endgültig und schreibgeschützt/)).toBeTruthy()
  // Same language as the interface: no "shown as generated" note is needed.
  expect(screen.queryByText(/genau so angezeigt/)).toBeNull()
  expect(screen.getByTestId('ai-suggestion').getAttribute('lang')).toBe('de')
  expect(screen.queryByRole('button')).toBeNull()
})

test('a ticket AI never applied to shows no AI section', () => {
  vi.stubGlobal('fetch', vi.fn())
  step({ status: 'queued', ai: null, transitions: [{ id: 1, event_type: 'submit', to_status: 'queued', payload: { route: 'ai_disabled' } }] })
  expect(screen.queryByRole('heading')).toBeNull()
})

const pageData = (ticket: object) =>
  ({ ticket, activity: { version: 'v1' }, aiPolicy: policy(), locale: 'en', policy: { allow_reopening: false, requester_cancellation_policy: 'anytime_while_open' }, conversation: { items: [], next_cursor: null }, unavailable: false })
const page = (ticket: object) => render(Page, { props: { data: pageData(ticket), params: {} } as never })

test('the page follows the server while the step is undecided and stops polling once it is settled or left', async () => {
  vi.useFakeTimers()
  const fetch = vi.fn().mockImplementation(async () => new Response('{"version":"v1"}'))
  vi.stubGlobal('fetch', fetch)
  const waiting = { ...base, status: 'ai_processing', ai: attempt({ state: 'processing', suggestion: null }) }
  const view = page(waiting)
  await vi.advanceTimersByTimeAsync(AI_POLL_MS)
  expect(fetch).toHaveBeenCalledTimes(1)
  expect(fetch.mock.calls[0][0]).toBe('/api/tickets/ticket-1/activity')
  // Unchanged marker: nothing reloads. The poll only reads; it never posts a decision.
  expect(invalidateAll).not.toHaveBeenCalled()
  fetch.mockImplementation(async () => new Response('{"version":"v2"}'))
  await vi.advanceTimersByTimeAsync(AI_POLL_MS)
  expect(invalidateAll).toHaveBeenCalledTimes(1)
  expect(posted(fetch)).toEqual([])

  // Settled: handed off to support. No more requests.
  const before = fetch.mock.calls.length
  await view.rerender({ data: pageData({ ...waiting, status: 'queued', ai: attempt({ state: 'handed_off', suggestion: null, reason: 'timeout' }) }) } as never)
  await vi.advanceTimersByTimeAsync(AI_POLL_MS * 3)
  expect(fetch.mock.calls.length).toBe(before)

  // Leaving an undecided page clears its timer.
  view.unmount()
  const left = page(base)
  await vi.advanceTimersByTimeAsync(AI_POLL_MS)
  const calls = fetch.mock.calls.length
  expect(calls).toBeGreaterThan(before)
  left.unmount()
  await vi.advanceTimersByTimeAsync(AI_POLL_MS * 3)
  expect(fetch.mock.calls.length).toBe(calls)
})
