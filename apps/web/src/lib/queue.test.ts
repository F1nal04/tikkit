import type { Role } from '@tikkit/api-client'
import { expect, test } from 'vitest'
import { age, parseQueue, queueHref, queueQuery, viewsFor } from '#lib/queue.ts'

const parse = (search: string, role: Role = 'operator') => parseQueue(new URLSearchParams(search), role)

test('defaults to open general-queue work, newest first', () => {
  expect(parse('')).toEqual({
    view: 'general',
    status: '',
    category_id: '',
    priority_id: '',
    unassigned: false,
    sort: 'newest',
    cursor: '',
  })
})

test('reads filters from the address and ignores unknown values', () => {
  const p = parse('view=team&status=closed&unassigned=true&sort=priority&category_id=c1&cursor=abc')
  expect(p).toMatchObject({ view: 'team', status: 'closed', unassigned: true, sort: 'priority', category_id: 'c1', cursor: 'abc' })
  expect(parse('view=nope&status=ai_solved&sort=drop')).toMatchObject({ view: 'general', status: '', sort: 'newest' })
})

test('only Admins open the overview; only others open assigned-to-me', () => {
  expect(viewsFor('operator')).toEqual(['general', 'team', 'mine'])
  expect(viewsFor('admin')).toEqual(['general', 'team', 'all'])
  expect(parse('view=all', 'operator').view).toBe('general')
  expect(parse('view=all', 'admin').view).toBe('all')
  expect(parse('view=mine', 'admin').view).toBe('general')
})

test('links and API queries carry only non-default values', () => {
  expect(queueHref({})).toBe('/queue')
  expect(queueHref({ view: 'mine', status: 'queued', unassigned: true })).toBe('/queue?view=mine&status=queued&unassigned=true')
  expect(queueQuery({ view: 'general', sort: 'newest', cursor: '' })).toEqual({})
})

test('age uses minutes, then hours, then days', () => {
  const now = Date.parse('2026-10-20T12:00:00Z')
  expect(age('2026-10-20T11:59:30Z', now)).toEqual({ value: 0, unit: 'minute' })
  expect(age('2026-10-20T11:15:00Z', now)).toEqual({ value: 45, unit: 'minute' })
  expect(age('2026-10-20T07:00:00Z', now)).toEqual({ value: 5, unit: 'hour' })
  expect(age('2026-10-17T12:00:00Z', now)).toEqual({ value: 3, unit: 'day' })
  expect(age('2026-10-21T12:00:00Z', now)).toEqual({ value: 0, unit: 'minute' })
})
