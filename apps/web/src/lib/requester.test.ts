import { expect, test } from 'vitest'
import type { Schemas, TicketDetail } from '@tikkit/api-client'
import { historyFilters, historyHref, historyQuery, requesterActions } from './requester.ts'

const policy = { allow_reopening: false, requester_cancellation_policy: 'anytime_while_open' } as Schemas['OrganizationSettings']
const ticket = (status: TicketDetail['status'], transitions: TicketDetail['transitions'] = []) => ({ status, view: 'requester', transitions }) as TicketDetail

test('history filters keep stable status and valid UTC calendar dates across pages', () => {
  const f = historyFilters(new URLSearchParams('status=ai_solved&from=2024-02-29&until=2024-03-02&cursor=next'))
  expect(historyQuery(f)).toEqual({ status: 'ai_solved', created_from: '2024-02-29T00:00:00Z', created_before: '2024-03-03T00:00:00.000Z', cursor: 'next', limit: 25 })
  expect(historyHref({ ...f, cursor: '' })).toBe('/?status=ai_solved&from=2024-02-29&until=2024-03-02')
  expect(historyFilters(new URLSearchParams('status=bogus&from=2023-02-29&until=bad'))).toEqual({ status: '', from: '', until: '', cursor: '' })
})

test('rejection is independent from reopening; final records have no actions', () => {
  expect(requesterActions(ticket('awaiting_confirmation'), policy)).toEqual({ reply: true, resolution: true, reopen: false, cancel: true })
  expect(requesterActions(ticket('closed'), policy).reopen).toBe(false)
  expect(requesterActions(ticket('closed'), { ...policy, allow_reopening: true }).reopen).toBe(true)
  for (const status of ['ai_solved', 'cancelled'] as const) {
    expect(requesterActions(ticket(status), { ...policy, allow_reopening: true })).toEqual({ reply: false, resolution: false, reopen: false, cancel: false })
  }
})

test('cancellation before work uses permanent transition history, not assignment or current state', () => {
  const before = { ...policy, requester_cancellation_policy: 'before_work_starts' } as const
  expect(requesterActions(ticket('queued'), before).cancel).toBe(true)
  const t = ticket('waiting_for_requester', [{ to_status: 'in_progress' }] as TicketDetail['transitions'])
  expect(requesterActions(t, before).cancel).toBe(false)
  expect(requesterActions(t, policy).cancel).toBe(true)
  expect(requesterActions(t, { ...policy, requester_cancellation_policy: 'disabled' }).cancel).toBe(false)
  expect(requesterActions({ ...t, view: 'staff' }, policy)).toEqual({ reply: false, resolution: false, reopen: false, cancel: false })
})
