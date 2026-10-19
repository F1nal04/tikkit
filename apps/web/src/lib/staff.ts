import type { TicketDetail } from '@tikkit/api-client'

/** Body of a staff action; the page adds the `expected_updated_at` it loaded. */
export type StaffInput =
  | { action: 'start_work' | 'request_information' | 'reopen' }
  | { action: 'propose_resolution'; summary: string }
  | { action: 'set_priority'; priority_id: string }

/** Body of a team/assignee change; the page adds `expected_updated_at` too. */
export type AssignmentInput =
  | { action: 'return_to_general' | 'claim' | 'unassign' }
  | { action: 'dispatch'; team_id: string }
  | { action: 'assign'; assignee_account_id: string }

/** Why a staff viewer cannot handle the ticket (affordance text only; the API rechecks every write). */
export function readOnlyReason(ticket: TicketDetail) {
  const access = ticket.staff_access
  if (!access || access.can_handle) return null
  if (ticket.status === 'cancelled') return 'cancelled' as const
  if (ticket.status === 'closed') return access.can_reopen ? null : ('closed' as const)
  if (['ai_processing', 'awaiting_ai_decision', 'ai_solved'].includes(ticket.status)) return 'ai' as const
  if (!ticket.assignment.team_name) return 'general' as const
  if (ticket.assignment.assignee_display_name) return 'claimed' as const
  return 'other' as const
}

const ROUTES = ['ai_disabled', 'category_exempt', 'requester_skip', 'ai_failed', 'not_solved'] as const

/** How the ticket reached human support: the latest entry into `queued`. */
export function routeOf(ticket: TicketDetail) {
  const entry = ticket.transitions.findLast((e) => e.to_status === 'queued' && typeof e.payload?.route === 'string')
  const route = entry?.payload.route
  return ROUTES.find((r) => r === route) ?? null
}
