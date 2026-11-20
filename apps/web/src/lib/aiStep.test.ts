import { ApiError, type TicketDetail } from '@tikkit/api-client'
import { expect, test } from 'vitest'
import { aiAnnouncement, aiFailure, aiStep, aiStepPolls, intakeAi } from './aiStep.ts'

type Ai = NonNullable<TicketDetail['ai']>
const attempt = (over: Partial<Ai> = {}): Ai => ({
  state: 'processing', language: 'en', suggestion: null, reason: null,
  created_at: '2026-11-16T10:00:00Z', finished_at: null, decision: null, decided_at: null, content_removed: false, ...over,
})
const suggestion = { explanation: 'Restart it.', steps: ['Close Word', 'Open Word'] }
const ticket = (status: TicketDetail['status'], ai: Ai | null, route?: string) =>
  ({ status, ai, transitions: route ? [{ event_type: 'submit', payload: { route } }] : [] }) as unknown as TicketDetail
const conflict = (status: number, details?: object) =>
  new ApiError(status, { error: { code: status === 409 ? 'conflict' : 'forbidden', message: '', details: details as never }, request_id: 'r' })

test('the ticket status picks the step: waiting, one suggestion, or a final AI-solved record', () => {
  expect(aiStep(ticket('ai_processing', attempt()))).toEqual({ kind: 'processing' })
  expect(aiStep(ticket('awaiting_ai_decision', attempt({ state: 'suggestion_ready', suggestion })))).toEqual({ kind: 'suggestion' })
  expect(aiStep(ticket('ai_solved', attempt({ state: 'suggestion_ready', suggestion, decision: 'solved' })))).toEqual({ kind: 'solved' })
  // No decision is offered without the stored text to decide on.
  expect(aiStep(ticket('awaiting_ai_decision', attempt({ state: 'suggestion_ready' })))).toEqual({ kind: 'processing' })
})

test('every handoff names its reason and never returns to the decision', () => {
  for (const reason of ['timeout', 'requester_skip', 'ai_unavailable', 'ai_failed'] as const) {
    expect(aiStep(ticket('queued', attempt({ state: 'handed_off', reason })))).toEqual({ kind: 'handoff', reason })
  }
  expect(aiStep(ticket('queued', attempt({ state: 'handed_off' })))).toEqual({ kind: 'handoff', reason: 'ai_failed' })
  const escalated = attempt({ state: 'suggestion_ready', suggestion, decision: 'not_solved' })
  for (const status of ['queued', 'in_progress', 'closed', 'cancelled'] as const) {
    expect(aiStep(ticket(status, escalated))).toEqual({ kind: 'handoff', reason: 'not_solved' })
  }
})

test('a late result after a handoff does not bring the suggestion screen back', () => {
  // Status says the request is with operators; a suggestion on the attempt must not reopen the choice.
  const late = attempt({ state: 'suggestion_ready', suggestion })
  expect(aiStep(ticket('queued', late)).kind).toBe('handoff')
  expect(aiStep(ticket('in_progress', attempt({ state: 'handed_off', reason: 'timeout', suggestion }))))
    .toEqual({ kind: 'handoff', reason: 'timeout' })
})

test('tickets without an attempt show a receipt only when AI applied and was bypassed', () => {
  expect(aiStep(ticket('queued', null, 'category_exempt'))).toEqual({ kind: 'handoff', reason: 'category_exempt' })
  expect(aiStep(ticket('in_progress', null, 'requester_skip'))).toEqual({ kind: 'handoff', reason: 'requester_skip' })
  expect(aiStep(ticket('queued', null, 'ai_disabled'))).toEqual({ kind: 'none' })
  expect(aiStep(ticket('queued', null))).toEqual({ kind: 'none' })
})

test('cancelling during an AI stage is not a handoff', () => {
  expect(aiStep(ticket('cancelled', attempt({ state: 'cancelled' })))).toEqual({ kind: 'cancelled' })
  expect(aiStep(ticket('cancelled', attempt({ state: 'suggestion_ready', suggestion })))).toEqual({ kind: 'cancelled' })
})

test('only undecided AI stages poll', () => {
  expect((['processing', 'suggestion', 'solved', 'handoff', 'cancelled', 'none'] as const).filter(aiStepPolls)).toEqual(['processing', 'suggestion'])
})

test('announcements cover the changes that happen while the page is open', () => {
  expect(aiAnnouncement('processing', 'suggestion')).toBe('suggestionReady')
  expect(aiAnnouncement('processing', 'handoff')).toBe('handedOff')
  expect(aiAnnouncement('suggestion', 'handoff')).toBe('handedOff')
  expect(aiAnnouncement('suggestion', 'solved')).toBe('solved')
  expect(aiAnnouncement('suggestion', 'suggestion')).toBeNull()
  expect(aiAnnouncement('processing', 'cancelled')).toBeNull()
})

test('failed decisions: a refused skip, a lost race with its committed status, or a retryable error', () => {
  expect(aiFailure('skip_ai', conflict(403))).toEqual({ kind: 'skip_forbidden' })
  expect(aiFailure('ai_solved', conflict(409, { fields: {}, status: 'queued' }))).toEqual({ kind: 'conflict', status: 'queued' })
  expect(aiFailure('skip_ai', conflict(409, { fields: {}, status: 'awaiting_ai_decision' }))).toEqual({ kind: 'conflict', status: 'awaiting_ai_decision' })
  expect(aiFailure('ai_not_solved', conflict(403))).toEqual({ kind: 'conflict', status: null })
  const offline = new ApiError(0, null)
  expect(aiFailure('ai_solved', offline)).toEqual({ kind: 'error', error: offline })
})

test('the form discloses AI and offers a skip only under the effective policy', () => {
  const on = { ai_assistance: true, requester_skip_allowed: false }
  expect(intakeAi({ ai_assistance: false, requester_skip_allowed: false }, { skip_ai: false }, true)).toEqual({ applies: false, exempt: false, maySkip: false, skip: false })
  expect(intakeAi(on, { skip_ai: false }, true)).toEqual({ applies: true, exempt: false, maySkip: false, skip: false })
  expect(intakeAi(on, undefined, false)).toEqual({ applies: true, exempt: false, maySkip: false, skip: false })
  const skippable = { ...on, requester_skip_allowed: true }
  expect(intakeAi(skippable, { skip_ai: false }, false)).toEqual({ applies: true, exempt: false, maySkip: true, skip: false })
  expect(intakeAi(skippable, { skip_ai: false }, true).skip).toBe(true)
  // An exempt category bypasses AI on its own; a skip is neither offered nor sent.
  expect(intakeAi(skippable, { skip_ai: true }, true)).toEqual({ applies: true, exempt: true, maySkip: false, skip: false })
})
