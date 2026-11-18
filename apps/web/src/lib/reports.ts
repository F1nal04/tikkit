import { calendarDate, dayRange } from '#lib/requester.ts'
import { uuidFilter } from '#lib/submissions.ts'

const DAY = 86_400_000

export function reportFilters(q: URLSearchParams, now: Date) {
  let from = calendarDate(q.get('from'))
  let until = calendarDate(q.get('until'))
  if (!q.has('from') && !q.has('until')) {
    until = now.toISOString().slice(0, 10)
    from = new Date(now.valueOf() - 29 * DAY).toISOString().slice(0, 10)
  }
  if (from && until && from > until) [from, until] = [until, from]
  return { from, until, category_id: uuidFilter(q.get('category_id')), team_id: uuidFilter(q.get('team_id')) }
}

export function reportQuery(f: ReturnType<typeof reportFilters>) {
  return { ...dayRange(f.from, f.until), category_id: f.category_id || undefined, team_id: f.team_id || undefined }
}

export function canExport(f: ReturnType<typeof reportFilters>) {
  const { created_from, created_before } = reportQuery(f)
  return !!(created_from && created_before && Date.parse(created_before) - Date.parse(created_from) <= 366 * DAY)
}
