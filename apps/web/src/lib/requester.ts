import type { Schemas, TicketDetail, TicketStatus } from '@tikkit/api-client'

export const HISTORY_STATUSES: TicketStatus[] = ['ai_processing', 'awaiting_ai_decision', 'queued', 'in_progress', 'waiting_for_requester', 'awaiting_confirmation', 'closed', 'cancelled', 'ai_solved']
export const isAiStage = (status: TicketStatus) => ['ai_processing', 'awaiting_ai_decision', 'ai_solved'].includes(status)
export const REPLY_STATUSES: TicketStatus[] = ['queued', 'in_progress', 'waiting_for_requester', 'awaiting_confirmation']
export type HistoryFilters = { status: string; from: string; until: string; cursor: string }

/** A valid `YYYY-MM-DD` calendar date, or `''`. */
export const calendarDate = (value: string | null) => {
  if (!value || !/^\d{4}-\d{2}-\d{2}$/.test(value)) return ''
  const parsed = new Date(`${value}T00:00:00Z`)
  return Number.isFinite(parsed.valueOf()) && parsed.toISOString().slice(0, 10) === value ? value : ''
}
export function historyFilters(q: URLSearchParams): HistoryFilters {
  return { status: HISTORY_STATUSES.includes(q.get('status') as TicketStatus) ? q.get('status')! : '', from: calendarDate(q.get('from')), until: calendarDate(q.get('until')), cursor: q.get('cursor') ?? '' }
}
/** Inclusive UTC calendar dates as the API's `created_from` / exclusive `created_before`. */
export function dayRange(from: string, until: string) {
  return {
    created_from: from ? `${from}T00:00:00Z` : undefined,
    created_before: until ? new Date(new Date(`${until}T00:00:00Z`).valueOf() + 86_400_000).toISOString() : undefined,
  }
}
export function historyQuery(f: HistoryFilters) {
  return {
    status: (f.status || undefined) as TicketStatus | undefined,
    ...dayRange(f.from, f.until),
    cursor: f.cursor || undefined,
    limit: 25,
  }
}
export function historyHref(f: HistoryFilters) {
  const query = new URLSearchParams(Object.entries(f).filter(([, v]) => v))
  return query.size ? `/?${query}` : '/'
}

/** Only affordances: the lifecycle rechecks ownership, state, and policy on every write. */
export function requesterActions(ticket: TicketDetail, policy: Schemas['OrganizationSettings']) {
  const own = ticket.view === 'requester'
  const open = REPLY_STATUSES.includes(ticket.status) || ticket.status === 'ai_processing' || ticket.status === 'awaiting_ai_decision'
  const started = ticket.transitions.some((e) => e.to_status === 'in_progress')
  return {
    reply: own && REPLY_STATUSES.includes(ticket.status),
    resolution: own && ticket.status === 'awaiting_confirmation',
    // Nothing is left to work on once the retention policy removed the content; the API refuses it too.
    reopen: own && ticket.status === 'closed' && policy.allow_reopening && !ticket.content_removed_at,
    cancel: own && open && (policy.requester_cancellation_policy === 'anytime_while_open' || (policy.requester_cancellation_policy === 'before_work_starts' && !started)),
  }
}

export function publicText(payload: unknown, field: string): string | null {
  const value = (payload as Record<string, unknown> | null)?.[field]
  return typeof value === 'string' ? value : null
}
