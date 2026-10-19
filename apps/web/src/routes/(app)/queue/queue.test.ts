import { fireEvent, render, screen, within } from '@testing-library/svelte'
import type { Role } from '@tikkit/api-client'
import { expect, test, vi } from 'vitest'
import { parseQueue } from '#lib/queue.ts'
import Page from './+page.svelte'

const { invalidateAll } = vi.hoisted(() => ({ invalidateAll: vi.fn().mockResolvedValue(undefined) }))
vi.mock('$app/navigation', () => ({ invalidateAll }))

const ticket = (n: number, extra = {}) => ({
  id: `00000000-0000-4000-8000-00000000000${n}`,
  reference: 100 + n,
  status: 'queued',
  subject: `Printer ${n}`,
  category_name: 'Hardware',
  priority_id: 'p1',
  priority_name: 'Urgent',
  requester: { id: 'r1', username: 'rita', display_name: 'Rita Requester' },
  assignment: { team_name: null, assignee_display_name: null },
  created_at: '2026-10-20T10:00:00Z',
  updated_at: '2026-10-20T10:00:00Z',
  ...extra,
})
const catalog = {
  categories: [{ id: 'c1', name: 'Hardware', skip_ai: false, archived: false }],
  priorities: [{ id: 'p1', name: 'Urgent', archived: false }],
  fields: [],
}

function show(search = '', role: Role = 'operator', over: Record<string, unknown> = {}, locale = 'en') {
  const data = {
    locale,
    user: { role },
    params: parseQueue(new URLSearchParams(search), role),
    page: { items: [ticket(1), ticket(2, { assignment: { team_name: 'Desk', assignee_display_name: 'Olga' }, status: 'in_progress' })], next_cursor: null },
    counts: { general: 2, team: 5, mine: 1, all: null },
    catalog,
    views: [],
    options: { teams: [{ id: 't1', name: 'Desk' }], assignees: [] },
    searchError: null,
    unavailable: false,
    ...over,
  }
  render(Page, { props: { data, params: {} } as never })
}

test('lists the columns operators need in an accessible table', () => {
  show()
  expect(screen.getByRole('heading', { level: 1 }).textContent).toBe('General queue')
  const table = screen.getByRole('table', { name: 'General queue: Tickets' })
  expect(within(table).getAllByRole('columnheader').map((h) => h.textContent)).toEqual([
    'Reference', 'Subject', 'Category', 'Requester', 'Priority', 'Age', 'Assignee', 'Status',
  ])
  const rows = within(table).getAllByRole('row').slice(1)
  expect(rows).toHaveLength(2)
  expect(within(rows[0]).getByRole('rowheader').textContent).toBe('#101')
  expect(rows[0].textContent).toContain('Rita Requester')
  expect(rows[0].textContent).toContain('Unassigned')
  expect(rows[0].textContent).toContain('Queued')
})

test('team views add the team column and show who has claimed a ticket', () => {
  show('view=team')
  const table = screen.getByRole('table')
  expect(within(table).getAllByRole('columnheader').map((h) => h.textContent)).toContain('Team')
  expect(within(table).getAllByRole('row')[2].textContent).toContain('Desk')
  expect(within(table).getAllByRole('row')[2].textContent).toContain('Olga')
  expect(screen.getByRole('heading', { level: 1 }).textContent).toBe('Team queue')
})

test('tabs carry counts and mark the current queue; Admins get the overview instead of assigned-to-me', () => {
  show('view=team')
  const tabs = within(screen.getByRole('navigation', { name: 'Queues' })).getAllByRole('link')
  expect(tabs.map((a) => a.textContent?.replace(/\s+/g, ' ').trim())).toEqual(['General queue 2', 'Team queue 5', 'Assigned to me 1'])
  expect(tabs[1].getAttribute('aria-current')).toBe('page')
  expect(tabs[0].getAttribute('href')).toBe('/queue')
  expect(tabs[2].getAttribute('href')).toBe('/queue?view=mine')
})

test('Admins see every team and the overview', () => {
  show('view=all', 'admin', { counts: { general: 2, team: 9, mine: 0, all: 11 } })
  const tabs = within(screen.getByRole('navigation', { name: 'Queues' })).getAllByRole('link')
  expect(tabs.map((a) => a.textContent?.replace(/\s+/g, ' ').trim())).toEqual(['General queue 2', 'All teams 9', 'Everything 11'])
})

test('filters reflect the address and submit as a plain GET', () => {
  show('view=mine&status=closed&unassigned=true&sort=priority&priority_id=p1')
  const form = screen.getByRole('form', { name: 'Filter and sort' }) as HTMLFormElement
  expect(form.method).toBe('get')
  expect((screen.getByLabelText('Status') as HTMLSelectElement).value).toBe('closed')
  expect((screen.getByLabelText('Priority') as HTMLSelectElement).value).toBe('p1')
  expect((screen.getByLabelText('Sort by') as HTMLSelectElement).value).toBe('priority')
  expect((screen.getByLabelText('Unassigned only') as HTMLInputElement).checked).toBe(true)
  expect((form.elements.namedItem('view') as HTMLInputElement).value).toBe('mine')
})

test('pages with first and next links that keep the filters', () => {
  show('status=queued&cursor=abc', 'operator', { page: { items: [ticket(1)], next_cursor: 'def' } })
  const pager = screen.getByRole('navigation', { name: 'Pages' })
  expect(within(pager).getByRole('link', { name: 'First page' }).getAttribute('href')).toBe('/queue?status=queued')
  expect(within(pager).getByRole('link', { name: 'Next page' }).getAttribute('href')).toBe('/queue?status=queued&cursor=def')
})

test('says so when a queue is empty', () => {
  show('view=mine', 'operator', { page: { items: [], next_cursor: null } })
  expect(screen.getByText('No tickets are assigned to you.')).toBeTruthy()
  expect(screen.queryByRole('table')).toBeNull()
})

test('recovers from an unavailable API with a retry', async () => {
  show('', 'operator', { page: null, counts: null, catalog: null, unavailable: true })
  expect(screen.getByRole('alert').textContent).toBe('The service is unavailable. Please try again later.')
  await fireEvent.click(screen.getByRole('button', { name: 'Try again' }))
  expect(invalidateAll).toHaveBeenCalled()
})

test('renders German labels and stable status identifiers', () => {
  show('', 'operator', {}, 'de')
  expect(screen.getByRole('heading', { level: 1 }).textContent).toBe('Allgemeine Warteschlange')
  expect(screen.getByRole('columnheader', { name: 'Referenz' })).toBeTruthy()
  expect(screen.getAllByText('Nicht zugewiesen').length).toBeGreaterThan(0)
  expect(document.querySelector('[data-status="queued"]')?.textContent).toBe('In der Warteschlange')
})

test('polls the counts marker and reloads the queue only when it changed', async () => {
  vi.useFakeTimers()
  try {
    const fetch = vi.fn().mockImplementation(async () => new Response(JSON.stringify({ general: 2, team: 5, mine: 1, all: null, version: 'v1' })))
    vi.stubGlobal('fetch', fetch)
    show('', 'operator', { counts: { general: 2, team: 5, mine: 1, all: null, version: 'v1' } })
    await vi.advanceTimersByTimeAsync(15_000)
    expect(String(fetch.mock.calls[0][0].url ?? fetch.mock.calls[0][0])).toContain('/api/queue/counts')
    expect(invalidateAll).not.toHaveBeenCalled()
    fetch.mockImplementation(async () => new Response(JSON.stringify({ general: 3, team: 5, mine: 1, all: null, version: 'v2' })))
    await vi.advanceTimersByTimeAsync(15_000)
    expect(invalidateAll).toHaveBeenCalledTimes(1)
  } finally {
    vi.useRealTimers()
    vi.unstubAllGlobals()
    invalidateAll.mockClear()
  }
})

test('searches through the filter form and shows bounds errors at the field', () => {
  show('q=printer')
  const search = screen.getByRole('searchbox', { name: 'Search' }) as HTMLInputElement
  expect(search.value).toBe('printer')
  expect(search.getAttribute('aria-describedby')).toBe('q-hint')
  expect(search.form?.getAttribute('aria-label')).toBe('Filter and sort')
  document.body.innerHTML = ''
  show('q=ab', 'operator', { searchError: 'too_short', page: { items: [], next_cursor: null } })
  const invalid = screen.getByRole('searchbox', { name: 'Search' })
  expect(invalid.getAttribute('aria-invalid')).toBe('true')
  expect(screen.getByText('This value is too short.')).toBeTruthy()
})

test('lists saved views, skipping stale filters, and saves the current filters', async () => {
  const fetch = vi.fn().mockImplementation(async () => new Response(JSON.stringify({}), { status: 201 }))
  vi.stubGlobal('fetch', fetch)
  try {
    const views = [
      { id: 'v1', name: 'Urgent VPN', created_at: '', filters: { view: 'team', priority_id: 'p1', category_id: 'gone', q: 'vpn' }, stale: ['category_id'] },
    ]
    show('view=team&q=printer&status=queued', 'operator', { views })
    const section = screen.getByRole('region', { name: 'Saved views' })
    expect(within(section).getByRole('link', { name: 'Urgent VPN' }).getAttribute('href')).toBe('/queue?view=team&priority_id=p1&q=vpn')
    expect(section.textContent).toContain('Some filters no longer apply and are skipped: category.')
    expect(within(section).getByRole('button', { name: 'Delete view Urgent VPN' })).toBeTruthy()
    await fireEvent.input(screen.getByLabelText('View name'), { target: { value: 'Printers' } })
    await fireEvent.submit(screen.getByRole('form', { name: 'Save these filters as a view' }))
    const [url, init] = fetch.mock.calls[0]
    expect(String(url)).toContain('/api/saved-views')
    expect(JSON.parse(init.body)).toEqual({ name: 'Printers', filters: { view: 'team', status: 'queued', q: 'printer' } })
    expect(await screen.findByText('View saved.')).toBeTruthy()
    expect(invalidateAll).toHaveBeenCalled()
  } finally {
    vi.unstubAllGlobals()
    invalidateAll.mockClear()
  }
})

test('bulk actions send each ticket with its listed version and report failures per ticket', async () => {
  const fetch = vi.fn().mockImplementation(async () =>
    new Response(JSON.stringify({ results: [
      { id: ticket(1).id, error: null },
      { id: ticket(2).id, error: 'forbidden' },
    ] })),
  )
  vi.stubGlobal('fetch', fetch)
  try {
    show()
    expect(screen.queryByRole('form', { name: 'Change selected tickets' })).toBeNull()
    await fireEvent.click(screen.getByRole('checkbox', { name: 'Select all tickets on this page' }))
    const bar = screen.getByRole('form', { name: 'Change selected tickets' })
    expect(bar.textContent).toContain('2 selected')
    const action = within(bar).getByLabelText('Action') as HTMLSelectElement
    // Operators get no assignment management; Admin-only overview never offers claiming to Admins.
    expect([...action.options].map((o) => o.value)).toEqual(['start_work', 'request_information', 'set_priority', 'claim', 'dispatch', 'return_to_general'])
    await fireEvent.change(action, { target: { value: 'dispatch' } })
    await fireEvent.change(within(bar).getByLabelText('Team'), { target: { value: 't1' } })
    await fireEvent.submit(bar)
    const [url, init] = fetch.mock.calls[0]
    expect(String(url)).toContain('/api/tickets/bulk')
    expect(JSON.parse(init.body)).toEqual({
      change: { action: 'dispatch', team_id: 't1' },
      tickets: [
        { id: ticket(1).id, expected_updated_at: '2026-10-20T10:00:00Z' },
        { id: ticket(2).id, expected_updated_at: '2026-10-20T10:00:00Z' },
      ],
    })
    const status = await screen.findByText(/1 of 2 tickets changed/)
    expect(status.textContent).toContain('#102: you may not do this on this ticket')
    expect(invalidateAll).toHaveBeenCalled()
    // The failed ticket stays selected for a retry.
    expect((screen.getByRole('checkbox', { name: 'Select #102' }) as HTMLInputElement).checked).toBe(true)
    expect((screen.getByRole('checkbox', { name: 'Select #101' }) as HTMLInputElement).checked).toBe(false)
  } finally {
    vi.unstubAllGlobals()
    invalidateAll.mockClear()
  }
})

test('leads and Admins get assignment in the bulk menu; Admins never claim', async () => {
  const options = { teams: [], assignees: [{ id: 'a1', display_name: 'Olga', team_id: 't1', team_name: 'Desk' }] }
  show('view=all', 'admin', { options, counts: { general: 2, team: 9, mine: 0, all: 11 } })
  await fireEvent.click(screen.getByRole('checkbox', { name: 'Select #101' }))
  const action = screen.getByLabelText('Action') as HTMLSelectElement
  expect([...action.options].map((o) => o.value)).toEqual(['start_work', 'request_information', 'set_priority', 'dispatch', 'return_to_general', 'assign', 'unassign'])
  await fireEvent.change(action, { target: { value: 'assign' } })
  expect([...(screen.getByLabelText('Assignee') as HTMLSelectElement).options].map((o) => o.textContent)).toEqual(['Choose…', 'Olga (Desk)'])
})
