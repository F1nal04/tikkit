import type { Schemas, TicketStatus } from '@tikkit/api-client'
import { calendarDate, dayRange, HISTORY_STATUSES } from '#lib/requester.ts'

/** Admin submission view (docs/admin-submissions.md). The address bar is the whole filter state. */
export const SOURCES = ['ai', 'operator', 'none'] as const satisfies readonly Schemas['ResolutionSourceFilter'][]
export const AI_OUTCOMES = ['solved', 'awaiting_decision', 'processing', 'not_solved', 'skipped', 'failed', 'cancelled', 'none'] as const satisfies readonly Schemas['AiOutcomeFilter'][]
export const STATUSES: readonly TicketStatus[] = HISTORY_STATUSES

export const DISPOSITIONS = ['solved', 'not_solved', 'provider_failed', 'failed', 'missing_attempt', 'category_exempt', 'ai_disabled', 'policy_withdrawn', 'requester_skip', 'processing', 'awaiting_decision', 'cancelled', 'unknown'] as const satisfies readonly Schemas['AiDisposition'][]
export const RESOLUTIONS = ['ai_confirmed', 'requester_confirmed', 'confirmation_timeout', 'cancelled', 'open'] as const satisfies readonly Schemas['SubmissionResolution'][]

export type SubmissionFilters = { category_id: string; team_id?: string; assignee_id?: string; entered_queue?: 'true' | 'false'; priority_id?: string; ai_disposition?: Schemas['AiDisposition']; resolution?: Schemas['SubmissionResolution']; lang?: string; status: string; resolution_source: string; ai_outcome: string; from: string; until: string; cursor: string }

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i
export const uuidFilter = (value: string | null) => value && UUID.test(value) ? value.toLowerCase() : ''
const oneOf = (values: readonly string[], value: string | null) => (value && values.includes(value) ? value : '')

/** Unknown values fall back to "all", so a hand-edited address never reaches the API as a 400. */
export function submissionFilters(q: URLSearchParams): SubmissionFilters {
  let from = calendarDate(q.get('from'))
  let until = calendarDate(q.get('until'))
  if (from && until && from > until) [from, until] = [until, from]
  const team = uuidFilter(q.get('team_id'))
  const priority = uuidFilter(q.get('priority_id'))
  const assignee = uuidFilter(q.get('assignee_id'))
  const enteredQueue = q.get('entered_queue')
  return {
    category_id: uuidFilter(q.get('category_id')),
    ...(team && { team_id: team }),
    ...(assignee && { assignee_id: assignee }),
    ...((enteredQueue === 'true' || enteredQueue === 'false') && { entered_queue: enteredQueue }),
    ...(priority && { priority_id: priority }),
    ...(oneOf(DISPOSITIONS, q.get('ai_disposition')) && { ai_disposition: q.get('ai_disposition') as Schemas['AiDisposition'] }),
    ...(oneOf(RESOLUTIONS, q.get('resolution')) && { resolution: q.get('resolution') as Schemas['SubmissionResolution'] }),
    ...(oneOf(['en', 'de'], q.get('lang')) && { lang: q.get('lang')! }),
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
    ...(f.team_id && { team_id: f.team_id }),
    ...(f.assignee_id && { assignee_id: f.assignee_id }),
    ...(f.entered_queue && { entered_queue: f.entered_queue === 'true' }),
    ...(f.priority_id && { priority_id: f.priority_id }),
    ...(f.ai_disposition && { ai_disposition: f.ai_disposition }),
    ...(f.resolution && { resolution: f.resolution }),
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

/**
 * Whether the ticket is help desk work an Admin can also open in the staff view. A ticket
 * cancelled before any handoff (AI outcome `cancelled`) never reached the help desk.
 */
export const inHelpDesk = (s: Pick<Schemas['SubmissionSummary'], 'status' | 'ai_outcome'>) =>
  !['ai_processing', 'awaiting_ai_decision', 'ai_solved', 'draft'].includes(s.status) && !(s.status === 'cancelled' && s.ai_outcome === 'cancelled')
