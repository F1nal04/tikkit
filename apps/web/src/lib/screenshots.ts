import { ApiError, CSRF_HEADER, type ErrorEnvelope, type Schemas } from '@tikkit/api-client'
import { errorText, fieldText, type messages } from '#lib/i18n.ts'

export type Screenshot = Schemas['Screenshot']
type Messages = (typeof messages)['en']

/** Mirrors the server limits in docs/screenshots.md; the server stays authoritative. */
export const ACCEPT = ['image/png', 'image/jpeg', 'image/webp']
export const MAX_BYTES = 10 * 1024 * 1024
export const MAX_PER_TICKET = 10

/** Upload one file as the raw body; the server validates and re-encodes it. */
export async function uploadScreenshot(file: File, fetchImpl = globalThis.fetch): Promise<Screenshot> {
  let response: Response
  try {
    response = await fetchImpl(`/api/screenshots?filename=${encodeURIComponent(file.name)}`, {
      method: 'POST',
      headers: { ...CSRF_HEADER, accept: 'application/json', 'content-type': file.type },
      body: file,
      credentials: 'include',
    })
  } catch {
    throw new ApiError(0, null)
  }
  const data = await response.json().catch(() => null)
  if (!response.ok) throw new ApiError(response.status, (data as ErrorEnvelope | null)?.error ? data : null)
  return data as Screenshot
}

/** A message before uploading, for what the server would reject anyway. */
export function precheck(t: Messages, file: File, attached: number): string | null {
  if (attached >= MAX_PER_TICKET) return t.screenshots.errors.limit
  if (!ACCEPT.includes(file.type)) return t.screenshots.errors.unsupported_type
  if (file.size > MAX_BYTES) return t.screenshots.errors.payload_too_large
  return null
}

/** Localized reason a screenshot upload failed. */
export function uploadErrorText(t: Messages, error: unknown): string {
  const reasons = t.screenshots.errors as Record<string, string>
  if (error instanceof ApiError) {
    if (error.code === 'payload_too_large') return reasons.payload_too_large
    const code = error.fields.file?.[0] ?? error.fields.filename?.[0]
    if (code) return reasons[code] ?? fieldText(t, code) ?? t.errors.generic
  }
  return errorText(t, error)
}
