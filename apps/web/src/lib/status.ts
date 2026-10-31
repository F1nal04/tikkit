/** Stable ticket status identifiers (spec: Conversations and resolution). Labels live in the i18n `statuses` table. */
export const TICKET_STATUSES = [
  'draft',
  'ai_processing',
  'awaiting_ai_decision',
  'queued',
  'in_progress',
  'waiting_for_requester',
  'awaiting_confirmation',
  'closed',
  'cancelled',
  'ai_solved',
] as const

export type TicketStatus = (typeof TICKET_STATUSES)[number]
