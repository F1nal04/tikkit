import { render, screen, within } from '@testing-library/svelte'
import { expect, test } from 'vitest'
import Page from './+page.svelte'

const summary = (over: object = {}) => ({
  id: 'ticket-1', reference: 42, subject: 'Printer jams', status: 'ai_solved', category_id: 'c1', category_name: 'Hardware', priority_name: 'Normal',
  requester: { id: 'r1', username: 'rita', display_name: 'Rita Requester' }, team_name: null, assignee_display_name: null,
  resolution_source: 'ai', closure_reason: 'requester_confirmed', resolution: 'ai_confirmed', route: 'ai', ai_outcome: 'solved',
  created_at: '2026-10-01T10:00:00Z', updated_at: '2026-10-01T10:05:00Z', ...over,
})
const ai = (over: object = {}) => ({
  attempt_id: 'a1', outcome: 'solved', state: 'suggestion_ready', outcome_code: null, provider: 'openai', model: 'model-a', language: 'de',
  suggestion: { explanation: 'Profil <img src=x onerror="window.x=1"> zurücksetzen', steps: ['Word <script>window.x=1</script> schließen', 'Neu starten'] },
  decision: 'solved', decision_event_id: 3, decided_at: '2026-10-01T10:05:00Z', created_at: '2026-10-01T10:00:00Z', finished_at: '2026-10-01T10:01:00Z', ...over,
})
const event = (id: number, event_type: string, from: string | null, to: string | null, actor: object | null, payload = {}) =>
  ({ id, event_type, from_status: from, to_status: to, actor, payload, created_at: '2026-10-01T10:00:00Z' })
const rita = { id: 'r1', username: 'rita', display_name: 'Rita Requester' }
const solvedEvents = [
  event(1, 'submit', 'draft', 'ai_processing', rita, { route: 'ai' }),
  event(2, 'ai_suggestion_ready', 'ai_processing', 'awaiting_ai_decision', null),
  event(3, 'ai_solved', 'awaiting_ai_decision', 'ai_solved', rita, { resolution_source: 'ai', closure_reason: 'requester_confirmed' }),
]
const intake = { subject: 'Printer jams', description: 'Line one\nLine <b>two</b>', catalog: { category_id: 'c1', category_name: 'Hardware', skip_ai: false, priority_id: 'p1', priority_name: 'Urgent', answers: [{ field_id: 'f1', label: 'Program', choice_id: null, value: 'Word' }] }, screenshot_ids: ['s1'] }
function show(submission: object, locale = 'en') {
  const data = { locale, submission: { description: 'Line one', intake_snapshot: intake, confirmation_deadline: null, ai: ai(), events: solvedEvents, summary: summary(), ...submission } }
  return render(Page, { props: { data, params: { id: 'ticket-1' } } as never })
}

test('an AI-solved record shows intake, suggestion as plain text, and the confirmation event, read-only', () => {
  show({})
  expect(screen.getByRole('heading', { level: 1, name: '#42 · Printer jams' })).toBeTruthy()
  expect(screen.getByTestId('resolution').textContent).toBe('Solved by AI, confirmed by the requester')
  expect(screen.getByTestId('ai-notice').textContent).toContain('explicitly confirmed')
  expect(screen.getByTestId('description').textContent).toBe('Line one\nLine <b>two</b>')
  expect(screen.getByText('Urgent')).toBeTruthy() // the original priority
  expect(screen.getByText('Word')).toBeTruthy()
  expect(screen.getByRole('img', { name: 'Screenshot 1' }).getAttribute('src')).toBe('/api/screenshots/s1/preview')
  const suggestion = screen.getByTestId('suggestion')
  expect(suggestion.getAttribute('lang')).toBe('de')
  expect(suggestion.querySelector('img, script')).toBeNull()
  expect(within(suggestion).getByText('Profil <img src=x onerror="window.x=1"> zurücksetzen')).toBeTruthy()
  expect(screen.getByTestId('decision').textContent).toBe('Solved')
  expect(screen.getByRole('link', { name: '#3' }).getAttribute('href')).toBe('#event-3')
  const confirmation = document.getElementById('event-3') as HTMLElement
  expect(confirmation.textContent).toContain('AI solution confirmed')
  expect(confirmation.textContent).toContain('Rita Requester (rita)')
  expect((document.getElementById('event-2') as HTMLElement).textContent).toContain('System')
  // No action of any kind, and no link into the help desk for an AI-solved record.
  expect(screen.queryAllByRole('button')).toEqual([])
  expect(screen.queryByRole('link', { name: 'Open in the help desk view' })).toBeNull()
  expect(screen.getByText('Not sent to the help desk')).toBeTruthy()
})

test('unconfirmed, abandoned, and failed attempts are not presented as solved', () => {
  const cases = [
    { outcome: 'awaiting_decision', status: 'awaiting_ai_decision', resolution: 'open', text: 'Unconfirmed: the requester has not decided' },
    { outcome: 'cancelled', status: 'cancelled', resolution: 'cancelled', text: 'Abandoned: the requester cancelled the ticket' },
    { outcome: 'failed', status: 'queued', resolution: 'open', text: 'Failed: no suggestion was created', code: 'timeout' },
  ]
  for (const c of cases) {
    const view = show({
      summary: summary({ status: c.status, resolution: c.resolution, ai_outcome: c.outcome, resolution_source: null, closure_reason: null }),
      ai: ai({ outcome: c.outcome, decision: null, decision_event_id: null, decided_at: null, outcome_code: c.code ?? null, ...(c.code ? { state: 'handed_off', suggestion: null } : {}) }),
      events: solvedEvents.slice(0, c.code ? 1 : 2),
    })
    const notice = screen.getByTestId('ai-notice')
    expect(notice.getAttribute('data-outcome')).toBe(c.outcome)
    expect(notice.textContent).toContain(c.text)
    expect(screen.getByTestId('decision').textContent).toBe('No decision')
    expect(screen.queryByText('Solved by AI, confirmed by the requester')).toBeNull()
    expect(screen.queryByText('Confirmation event')).toBeNull()
    if (c.code) {
      expect(screen.getByText('Took too long')).toBeTruthy()
      expect(screen.getByText('No suggestion was stored.')).toBeTruthy()
      expect(screen.getByRole('link', { name: 'Open in the help desk view' }).getAttribute('href')).toBe('/queue/ticket-1')
    }
    view.unmount()
  }
})

test('a help desk resolution without AI, in German', () => {
  show({
    summary: summary({ status: 'closed', resolution: 'confirmation_timeout', resolution_source: 'operator', closure_reason: 'confirmation_timeout', ai_outcome: null, route: 'ai_disabled', team_name: 'Desk', assignee_display_name: 'Olga' }),
    ai: null,
    events: [
      event(1, 'submit', 'draft', 'queued', rita, { route: 'ai_disabled' }),
      event(2, 'assignment_changed', null, null, { id: 'o1', username: 'olga', display_name: 'Olga' }, { action: 'claim' }),
      event(3, 'propose_resolution', 'in_progress', 'awaiting_confirmation', { id: 'o1', username: 'olga', display_name: 'Olga' }, { summary: 'Toner <i>getauscht</i>' }),
      event(4, 'confirmation_timeout', 'awaiting_confirmation', 'closed', null),
    ],
  }, 'de')
  expect(screen.getByTestId('resolution').textContent).toBe('Vom Helpdesk gelöst, ohne Bestätigung automatisch geschlossen')
  expect(screen.queryByTestId('ai-notice')).toBeNull()
  expect(screen.queryByRole('heading', { name: 'KI-Versuch' })).toBeNull()
  expect(screen.getByText('KI aus: direkt an den Helpdesk')).toBeTruthy()
  expect(screen.getByText('Zuweisung geändert')).toBeTruthy()
  expect(screen.getByText('Zusammenfassung: Toner <i>getauscht</i>')).toBeTruthy()
  expect(screen.getByRole('link', { name: 'In der Helpdesk-Ansicht öffnen' })).toBeTruthy()
})
