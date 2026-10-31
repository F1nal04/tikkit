import type { Schemas, TicketDetail, TicketStatus } from '@tikkit/api-client'

export const HISTORY_STATUSES: TicketStatus[] = ['ai_processing', 'awaiting_ai_decision', 'queued', 'in_progress', 'waiting_for_requester', 'awaiting_confirmation', 'closed', 'cancelled', 'ai_solved']
export const isAiStage = (status: TicketStatus) => ['ai_processing', 'awaiting_ai_decision', 'ai_solved'].includes(status)
export const REPLY_STATUSES: TicketStatus[] = ['queued', 'in_progress', 'waiting_for_requester', 'awaiting_confirmation']
export type HistoryFilters = { status: string; from: string; until: string; cursor: string }

const date = (value: string | null) => {
  if (!value || !/^\d{4}-\d{2}-\d{2}$/.test(value)) return ''
  const parsed = new Date(`${value}T00:00:00Z`)
  return Number.isFinite(parsed.valueOf()) && parsed.toISOString().slice(0, 10) === value ? value : ''
}
export function historyFilters(q: URLSearchParams): HistoryFilters {
  return { status: HISTORY_STATUSES.includes(q.get('status') as TicketStatus) ? q.get('status')! : '', from: date(q.get('from')), until: date(q.get('until')), cursor: q.get('cursor') ?? '' }
}
export function historyQuery(f: HistoryFilters) {
  return {
    status: (f.status || undefined) as TicketStatus | undefined,
    created_from: f.from ? `${f.from}T00:00:00Z` : undefined,
    created_before: f.until ? new Date(new Date(`${f.until}T00:00:00Z`).valueOf() + 86_400_000).toISOString() : undefined,
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
    reopen: own && ticket.status === 'closed' && policy.allow_reopening,
    cancel: own && open && (policy.requester_cancellation_policy === 'anytime_while_open' || (policy.requester_cancellation_policy === 'before_work_starts' && !started)),
  }
}

export function publicText(payload: unknown, field: string): string | null {
  const value = (payload as Record<string, unknown> | null)?.[field]
  return typeof value === 'string' ? value : null
}
