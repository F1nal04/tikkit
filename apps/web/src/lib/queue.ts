import type { components, paths, Role } from '@tikkit/api-client'

type ApiQuery = NonNullable<paths['/api/queue']['get']['parameters']['query']>
type SavedView = components['schemas']['SavedView']
type QueueFilters = components['schemas']['QueueFilters']

export const VIEWS = ['general', 'team', 'mine', 'all'] as const
export type QueueView = (typeof VIEWS)[number]
export const SORTS = ['newest', 'oldest', 'priority'] as const
export type QueueSort = (typeof SORTS)[number]
/** Statuses a queue can show; without one it shows all open work. Mirrors `FILTERABLE` in queues.rs. */
export const FILTER_STATUSES = [
  'queued',
  'in_progress',
  'waiting_for_requester',
  'awaiting_confirmation',
  'closed',
  'cancelled',
] as const
export type FilterStatus = (typeof FILTER_STATUSES)[number]

export type QueueParams = {
  view: QueueView
  status: FilterStatus | ''
  category_id: string
  priority_id: string
  unassigned: boolean
  sort: QueueSort
  /** Search text; the API checks its bounds. */
  q: string
  cursor: string
}

/** Tabs a role may open. Admins are never assignees; only they get the overview. Rust enforces it. */
export const viewsFor = (role: Role): QueueView[] =>
  role === 'admin' ? ['general', 'team', 'all'] : ['general', 'team', 'mine']

const oneOf = <T extends string>(allowed: readonly T[], value: string | null, fallback: T): T =>
  allowed.includes(value as T) ? (value as T) : fallback

/** Reads the address bar. Unknown values fall back to defaults, so an old link still renders. */
export function parseQueue(search: URLSearchParams, role: Role): QueueParams {
  const views = viewsFor(role)
  return {
    view: oneOf(views, search.get('view'), 'general'),
    status: oneOf(['', ...FILTER_STATUSES], search.get('status'), ''),
    category_id: search.get('category_id') ?? '',
    priority_id: search.get('priority_id') ?? '',
    unassigned: search.get('unassigned') === 'true',
    sort: oneOf(SORTS, search.get('sort'), 'newest'),
    q: search.get('q')?.trim() ?? '',
    cursor: search.get('cursor') ?? '',
  }
}

/** Only non-default values: the same query for the address bar and for the API. */
export function queueQuery(p: Partial<QueueParams>) {
  const query: ApiQuery = {}
  if (p.view && p.view !== 'general') query.view = p.view
  if (p.status) query.status = p.status
  if (p.category_id) query.category_id = p.category_id
  if (p.priority_id) query.priority_id = p.priority_id
  if (p.unassigned) query.unassigned = true
  if (p.sort && p.sort !== 'newest') query.sort = p.sort
  if (p.q) query.q = p.q
  if (p.cursor) query.cursor = p.cursor
  return query
}

export function queueHref(p: Partial<QueueParams>) {
  const search = new URLSearchParams(Object.entries(queueQuery(p)).map(([k, v]) => [k, String(v)])).toString()
  return search ? `/queue?${search}` : '/queue'
}

/** The current filters as a saved view stores them: no page cursor. */
export function viewFilters(p: QueueParams): QueueFilters {
  const { cursor: _, ...query } = queueQuery(p)
  return query
}

/** Where a saved view leads: its filters minus the ones the API reports as stale. */
export function savedViewHref(view: SavedView) {
  const f = view.filters
  const stale = new Set<string>(view.stale)
  const keep = <T>(key: string, value: T | null | undefined) => (stale.has(key) ? undefined : (value ?? undefined))
  return queueHref({
    view: keep('view', f.view),
    status: keep('status', f.status) as FilterStatus | undefined,
    category_id: keep('category_id', f.category_id),
    priority_id: keep('priority_id', f.priority_id),
    unassigned: keep('unassigned', f.unassigned),
    sort: keep('sort', f.sort),
    q: keep('q', f.q),
  })
}

/** Bulk changes, in menu order. Rust checks every ticket on its own. */
export const BULK_ACTIONS = [
  'start_work',
  'request_information',
  'set_priority',
  'claim',
  'dispatch',
  'return_to_general',
  'assign',
  'unassign',
] as const
export type BulkAction = (typeof BULK_ACTIONS)[number]
type BulkChange = components['schemas']['BulkChange']

/** Actions that need a chosen team, assignee, or priority. */
export const needsTarget = (action: BulkAction) => action === 'dispatch' || action === 'assign' || action === 'set_priority'

/** The request body's `change` for an action and its chosen target ID. */
export function bulkChange(action: BulkAction, target: string): BulkChange {
  switch (action) {
    case 'dispatch':
      return { action, team_id: target }
    case 'assign':
      return { action, assignee_account_id: target }
    case 'set_priority':
      return { action, priority_id: target }
    default:
      return { action }
  }
}

/** Time since creation as a number and unit for `Intl.NumberFormat`. */
export function age(iso: string, now: number): { value: number; unit: 'minute' | 'hour' | 'day' } {
  const minutes = Math.max(0, Math.floor((now - Date.parse(iso)) / 60000))
  if (minutes < 60) return { value: minutes, unit: 'minute' }
  if (minutes < 48 * 60) return { value: Math.floor(minutes / 60), unit: 'hour' }
  return { value: Math.floor(minutes / 1440), unit: 'day' }
}
