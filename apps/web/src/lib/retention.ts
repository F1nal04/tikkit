import { ApiError, CSRF_HEADER, type ErrorEnvelope, type Schemas } from '@tikkit/api-client'

/** Admin data retention (docs/retention.md). */
export type RetentionPolicy = Schemas['RetentionPolicy']
export type RetentionCounts = Schemas['RetentionCounts']
export type RetentionRun = Schemas['RetentionRun']

/** The word an Admin types before an irreversible deletion. The API checks it; it is not translated. */
export const CONFIRM = 'DELETE'

export const PERIODS = ['draft_days', 'screenshot_days', 'ai_content_days', 'ticket_content_days', 'audit_days'] as const
export type Period = (typeof PERIODS)[number]
export const COUNTS = ['drafts', 'screenshots', 'ai_content', 'ticket_content', 'audit_events'] as const
export type PeriodForm = Record<Period, string>

/** A period as the input shows it: empty means "keep". */
export function periodForm(policy: RetentionPolicy): PeriodForm {
  return Object.fromEntries(PERIODS.map((key) => [key, policy[key] === null ? '' : String(policy[key])])) as PeriodForm
}

/**
 * The typed periods as the API takes them: empty is `null` (keep). Anything that is not a whole
 * number becomes `0`, which the API rejects as out of range, so a typo never silently means "keep".
 */
export function policyOf(form: PeriodForm): RetentionPolicy {
  const days = (raw: string) => {
    const text = String(raw ?? '').trim()
    if (text === '') return null
    return /^\d{1,6}$/.test(text) ? Number(text) : 0
  }
  return Object.fromEntries(PERIODS.map((key) => [key, days(form[key])])) as RetentionPolicy
}

export const samePolicy = (a: RetentionPolicy, b: RetentionPolicy) => PERIODS.every((key) => a[key] === b[key])
export const keepsEverything = (policy: RetentionPolicy) => PERIODS.every((key) => policy[key] === null)
export const totalOf = (counts: RetentionCounts) => COUNTS.reduce((sum, key) => sum + counts[key], 0)

export type PersonalDataExport = { blob: Blob; filename: string }

/** Downloads one account's personal data. The typed username must be that account's. */
export async function exportPersonalData(accountId: string, username: string, fetchImpl: typeof fetch = fetch): Promise<PersonalDataExport> {
  let response: Response
  try {
    response = await fetchImpl(`/api/admin/accounts/${accountId}/personal-data/export`, {
      method: 'POST',
      headers: { ...CSRF_HEADER, 'content-type': 'application/json' },
      credentials: 'include',
      body: JSON.stringify({ username }),
    })
  } catch {
    throw new ApiError(0, null)
  }
  if (!response.ok) {
    const body = (await response.json().catch(() => null)) as ErrorEnvelope | null
    throw new ApiError(response.status, typeof body?.error?.code === 'string' ? body : null)
  }
  const filename = /filename="([^"]+)"/.exec(response.headers.get('content-disposition') ?? '')?.[1] ?? `personal-data-${accountId}.json`
  return { blob: await response.blob(), filename }
}
