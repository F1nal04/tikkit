import { fireEvent, render, screen } from '@testing-library/svelte'
import { afterEach, expect, test, vi } from 'vitest'
import Page from './+page.svelte'

vi.mock('$app/navigation', () => ({ goto: vi.fn(), invalidateAll: vi.fn() }))
afterEach(() => vi.unstubAllGlobals())

const item = (id: string, read_at: string | null) => ({
  id, ticket_id: 't1', title: 'New public reply', body: 'For account ada. Ticket #7.', href: '/tickets/t1',
  created_at: '2026-01-01T10:00:00Z', read_at,
})
const data = (items: ReturnType<typeof item>[], locale: 'en' | 'de' = 'en') => ({
  locale, user: null, page: { items, next_cursor: null }, unavailable: false as const,
})

test('lists notifications with portal links and marks one read', async () => {
  const fetch = vi.fn().mockResolvedValue(new Response(null, { status: 204 }))
  vi.stubGlobal('fetch', fetch)
  render(Page, { props: { data: data([item('n1', null), item('n2', '2026-01-02T00:00:00Z')]), params: {} } })
  expect(screen.getAllByRole('link', { name: 'Open ticket' }).map((a) => a.getAttribute('href'))).toEqual(['/tickets/t1', '/tickets/t1'])
  // Only the unread entry offers the action.
  const buttons = screen.getAllByRole('button', { name: 'Mark as read' })
  expect(buttons).toHaveLength(1)
  await fireEvent.click(buttons[0])
  await vi.waitFor(() => expect(screen.queryByRole('button', { name: 'Mark as read' })).toBeNull())
  const [url, init] = fetch.mock.calls[0]
  expect(url).toBe('/api/notifications/n1/read')
  expect(init.method).toBe('POST')
})

test('shows localized empty and unavailable states', async () => {
  const { rerender } = render(Page, { props: { data: data([], 'de'), params: {} } })
  expect(screen.getByText('Keine Benachrichtigungen.')).toBeTruthy()
  await rerender({ data: { ...data([]), page: null, unavailable: true as const } })
  expect(screen.getByRole('alert').textContent).toContain('The service is unavailable.')
})
