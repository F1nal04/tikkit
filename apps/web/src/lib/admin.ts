/** Dropdowns must include every candidate, even when the API returns multiple pages. */
export async function allPages<T>(request: (cursor?: string) => Promise<{ items: T[]; next_cursor: string | null }>): Promise<T[]> {
  const items: T[] = []
  let cursor: string | undefined
  do {
    const page = await request(cursor)
    items.push(...page.items)
    cursor = page.next_cursor ?? undefined
  } while (cursor)
  return items
}

/** Browser timezone data can lag PostgreSQL; keep history readable with a labeled UTC fallback. */
export function invitationTime(iso: string, locale: string, timezone: string): string {
  let formatter: Intl.DateTimeFormat
  try { formatter = new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'short', timeZone: timezone }) }
  catch { timezone = 'UTC'; formatter = new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'short', timeZone: timezone }) }
  return `${formatter.format(new Date(iso))} (${timezone})`
}
