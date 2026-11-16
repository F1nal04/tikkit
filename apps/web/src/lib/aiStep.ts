import { ApiError, type Schemas, type TicketDetail, type TicketStatus } from '@tikkit/api-client'
import { publicText } from './requester.ts'

export type AiPolicy = Schemas['AiPolicy']
export type AiAction = 'ai_solved' | 'ai_not_solved' | 'skip_ai'
/** Why a request is with the support team instead of ending at the AI step. */
export type HandoffReason = Schemas['AiHandoffReason'] | 'not_solved' | 'category_exempt' | 'other'
export type AiStepView =
  | { kind: 'none' }
  | { kind: 'processing' }
  | { kind: 'suggestion' }
  | { kind: 'solved' }
  | { kind: 'cancelled' }
  | { kind: 'handoff'; reason: HandoffReason }

/** How often the requester page checks whether the AI step moved on. The server decides when. */
export const AI_POLL_MS = 3_000

type StepInput = Pick<TicketDetail, 'status' | 'ai' | 'transitions'>

/**
 * What the requester page shows for the AI step. The ticket status decides: once a ticket
 * left the AI stage, a stored or late suggestion never brings the decision back.
 */
export function aiStep(ticket: StepInput): AiStepView {
  const { status, ai } = ticket
  if (status === 'ai_processing') return { kind: 'processing' }
  // A decision needs the stored suggestion; without one the page keeps waiting for the server.
  if (status === 'awaiting_ai_decision') return { kind: ai?.suggestion ? 'suggestion' : 'processing' }
  if (status === 'ai_solved') return { kind: 'solved' }
  if (!ai) {
    // No attempt was made. The submission event says whether AI applied and was bypassed.
    const route = publicText(ticket.transitions.find((e) => e.event_type === 'submit')?.payload, 'route')
    if (route === 'category_exempt' || route === 'requester_skip') return { kind: 'handoff', reason: route }
    return { kind: 'none' }
  }
  if (ai.decision === 'not_solved') return { kind: 'handoff', reason: 'not_solved' }
  if (ai.state === 'handed_off') return { kind: 'handoff', reason: ai.reason ?? 'ai_failed' }
  if (status === 'cancelled') return { kind: 'cancelled' }
  return { kind: 'handoff', reason: 'other' }
}

/** Only undecided AI stages change without the requester; every other state is settled. */
export const aiStepPolls = (kind: AiStepView['kind']) => kind === 'processing' || kind === 'suggestion'

/** What a screen reader hears when the step changes while the page is open. */
export function aiAnnouncement(from: AiStepView['kind'], to: AiStepView['kind']) {
  if (from === to) return null
  if (to === 'suggestion') return 'suggestionReady' as const
  if (to === 'handoff') return 'handedOff' as const
  if (to === 'solved') return 'solved' as const
  return null
}

export type AiFailure =
  | { kind: 'skip_forbidden' }
  /** Another decision already committed; `status` is the ticket's committed status. */
  | { kind: 'conflict'; status: TicketStatus | null }
  | { kind: 'error'; error: unknown }

/** Sorts a failed decision request. `skip_forbidden` and `conflict` are answers: reload, do not retry. */
export function aiFailure(action: AiAction, error: unknown): AiFailure {
  if (error instanceof ApiError) {
    if (error.status === 403 && action === 'skip_ai') return { kind: 'skip_forbidden' }
    if (error.status === 409 || error.status === 403) return { kind: 'conflict', status: error.body?.error.details?.status ?? null }
  }
  return { kind: 'error', error }
}

/** Whether the form's AI disclosure applies, and whether a skip may be offered and sent. */
export function intakeAi(policy: AiPolicy, category: { skip_ai: boolean } | undefined, skipChosen: boolean) {
  const applies = policy.ai_assistance
  const exempt = applies && !!category?.skip_ai
  const maySkip = applies && !exempt && policy.requester_skip_allowed
  return { applies, exempt, maySkip, skip: maySkip && skipChosen }
}
