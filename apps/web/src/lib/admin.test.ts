import { expect, test, vi } from 'vitest'
import { allPages, invitationTime } from './admin'
test('loads all selector candidates through cursor pages', async () => {
  const request = vi.fn().mockResolvedValueOnce({ items: [1, 2], next_cursor: 'next' }).mockResolvedValueOnce({ items: [3], next_cursor: null })
  expect(await allPages(request)).toEqual([1, 2, 3])
  expect(request.mock.calls).toEqual([[undefined], ['next']])
})

test('invitation timestamps use the organization timezone and fall back for unsupported browser zones', () => {
  const iso = '2026-01-01T12:00:00Z'
  expect(invitationTime(iso, 'en', 'Europe/Berlin')).toContain('Europe/Berlin')
  expect(invitationTime(iso, 'en', 'Factory')).toBe(invitationTime(iso, 'en', 'UTC'))
})
