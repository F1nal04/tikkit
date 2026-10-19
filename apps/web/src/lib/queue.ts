import type { paths, Role } from '@tikkit/api-client'

type ApiQuery = NonNullable<paths['/api/queue']['get']['parameters']['query']>

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
  if (p.cursor) query.cursor = p.cursor
  return query
}

export function queueHref(p: Partial<QueueParams>) {
  const search = new URLSearchParams(Object.entries(queueQuery(p)).map(([k, v]) => [k, String(v)])).toString()
  return search ? `/queue?${search}` : '/queue'
}

/** Time since creation as a number and unit for `Intl.NumberFormat`. */
export function age(iso: string, now: number): { value: number; unit: 'minute' | 'hour' | 'day' } {
  const minutes = Math.max(0, Math.floor((now - Date.parse(iso)) / 60000))
  if (minutes < 60) return { value: minutes, unit: 'minute' }
  if (minutes < 48 * 60) return { value: Math.floor(minutes / 60), unit: 'hour' }
  return { value: Math.floor(minutes / 1440), unit: 'day' }
}
