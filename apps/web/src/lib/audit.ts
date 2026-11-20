import { ApiError, CSRF_HEADER, type ErrorEnvelope, type Schemas } from '@tikkit/api-client'
import { calendarDate, dayRange } from '#lib/requester.ts'

/** Admin audit log (docs/audit.md). The address bar is the whole filter state. */
export type AuditEvent = Schemas['AuditEvent']
export type AuditFilters = {
  actor_username: string
  action: string
  ticket_reference: string
  resource_type: string
  resource_id: string
  request_id: string
  from: string
  until: string
  cursor: string
}

const IDENTIFIER = /^[a-z0-9_]{1,64}$/
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i
const text = (value: string | null, max: number) => {
  const trimmed = (value ?? '').trim()
  return trimmed.length <= max ? trimmed : ''
}
const identifier = (value: string | null) => (IDENTIFIER.test(value ?? '') ? value! : '')

/** Values the API would reject fall back to "no filter", so a hand-edited address never becomes a 400. */
export function auditFilters(q: URLSearchParams): AuditFilters {
  let from = calendarDate(q.get('from'))
  let until = calendarDate(q.get('until'))
  if (from && until && from > until) [from, until] = [until, from]
  // People copy references as "#1042".
  const reference = (q.get('ticket_reference') ?? '').trim().replace(/^#/, '')
  return {
    actor_username: text(q.get('actor_username'), 64),
    action: identifier(q.get('action')),
    ticket_reference: /^[1-9]\d{0,14}$/.test(reference) ? reference : '',
    resource_type: identifier(q.get('resource_type')),
    resource_id: text(q.get('resource_id'), 128),
    // Request IDs are UUIDs (docs/operations.md).
    request_id: UUID.test((q.get('request_id') ?? '').trim()) ? q.get('request_id')!.trim().toLowerCase() : '',
    from,
    until,
    cursor: q.get('cursor') ?? '',
  }
}

/** The filters as API parameters, shared by the search and the export. */
export function auditParams(f: AuditFilters) {
  return {
    actor_username: f.actor_username || undefined,
    action: f.action || undefined,
    ticket_reference: f.ticket_reference ? Number(f.ticket_reference) : undefined,
    resource_type: f.resource_type || undefined,
    resource_id: f.resource_id || undefined,
    request_id: f.request_id || undefined,
    ...dayRange(f.from, f.until),
  }
}

export function auditQuery(f: AuditFilters) {
  return { ...auditParams(f), cursor: f.cursor || undefined, limit: 50 }
}

export function auditHref(f: Partial<AuditFilters>) {
  const query = new URLSearchParams(Object.entries(f).filter((entry): entry is [string, string] => !!entry[1]))
  return query.size ? `/admin/audit?${query}` : '/admin/audit'
}

/** Metadata is identifiers and settings; it is shown as text, never interpreted. */
export const metadataText = (metadata: object) => JSON.stringify(metadata, null, 2)
export const hasMetadata = (metadata: object) => Object.keys(metadata).length > 0

export type AuditExport = { blob: Blob; filename: string; rows: number; truncated: boolean }

/** Downloads the filtered log as CSV. The server bounds the size and records the export. */
export async function exportAudit(f: AuditFilters, fetchImpl: typeof fetch = fetch): Promise<AuditExport> {
  const query = new URLSearchParams()
  for (const [key, value] of Object.entries(auditParams(f))) if (value !== undefined) query.set(key, String(value))
  let response: Response
  try {
    response = await fetchImpl(`/api/admin/audit/export${query.size ? `?${query}` : ''}`, { method: 'POST', headers: CSRF_HEADER, credentials: 'include' })
  } catch {
    throw new ApiError(0, null)
  }
  if (!response.ok) {
    const body = (await response.json().catch(() => null)) as ErrorEnvelope | null
    throw new ApiError(response.status, typeof body?.error?.code === 'string' ? body : null)
  }
  const filename = /filename="([^"]+)"/.exec(response.headers.get('content-disposition') ?? '')?.[1] ?? 'tikkit-audit.csv'
  return {
    blob: await response.blob(),
    filename,
    rows: Number(response.headers.get('x-export-rows') ?? 0),
    truncated: response.headers.get('x-export-truncated') === 'true',
  }
}
