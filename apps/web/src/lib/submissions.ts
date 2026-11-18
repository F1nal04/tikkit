import type { Schemas, TicketStatus } from '@tikkit/api-client'
import { calendarDate, dayRange, HISTORY_STATUSES } from '#lib/requester.ts'

/** Admin submission view (docs/admin-submissions.md). The address bar is the whole filter state. */
export const SOURCES = ['ai', 'operator', 'none'] as const satisfies readonly Schemas['ResolutionSourceFilter'][]
export const AI_OUTCOMES = ['solved', 'awaiting_decision', 'processing', 'not_solved', 'skipped', 'failed', 'cancelled', 'none'] as const satisfies readonly Schemas['AiOutcomeFilter'][]
export const STATUSES: readonly TicketStatus[] = HISTORY_STATUSES

export type SubmissionFilters = { category_id: string; status: string; resolution_source: string; ai_outcome: string; from: string; until: string; cursor: string }

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i
const oneOf = (values: readonly string[], value: string | null) => (value && values.includes(value) ? value : '')

/** Unknown values fall back to "all", so a hand-edited address never reaches the API as a 400. */
export function submissionFilters(q: URLSearchParams): SubmissionFilters {
  let from = calendarDate(q.get('from'))
  let until = calendarDate(q.get('until'))
  if (from && until && from > until) [from, until] = [until, from]
  return {
    category_id: UUID.test(q.get('category_id') ?? '') ? q.get('category_id')!.toLowerCase() : '',
    status: oneOf(STATUSES, q.get('status')),
    resolution_source: oneOf(SOURCES, q.get('resolution_source')),
    ai_outcome: oneOf(AI_OUTCOMES, q.get('ai_outcome')),
    from,
    until,
    cursor: q.get('cursor') ?? '',
  }
}

export function submissionQuery(f: SubmissionFilters) {
  return {
    category_id: f.category_id || undefined,
    status: (f.status || undefined) as TicketStatus | undefined,
    resolution_source: (f.resolution_source || undefined) as Schemas['ResolutionSourceFilter'] | undefined,
    ai_outcome: (f.ai_outcome || undefined) as Schemas['AiOutcomeFilter'] | undefined,
    ...dayRange(f.from, f.until),
    cursor: f.cursor || undefined,
    limit: 25,
  }
}

export function submissionsHref(f: Partial<SubmissionFilters>) {
  const query = new URLSearchParams(Object.entries(f).filter((entry): entry is [string, string] => !!entry[1]))
  return query.size ? `/admin/submissions?${query}` : '/admin/submissions'
}

/** Only an explicit requester confirmation is an AI resolution; everything else stays distinguishable from it. */
export const isAiResolution = (outcome: Schemas['AiRecordOutcome'] | null | undefined) => outcome === 'solved'

/** Statuses whose ticket is help desk work an Admin can also open in the staff view. */
export const inHelpDesk = (status: TicketStatus) => !['ai_processing', 'awaiting_ai_decision', 'ai_solved', 'draft'].includes(status)
