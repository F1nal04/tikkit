import { calendarDate } from '#lib/requester.ts'
import { uuidFilter } from '#lib/submissions.ts'

const DAY = 86_400_000
const isoDay = (ms: number) => new Date(ms).toISOString().slice(0, 10)
const utcMidnight = (day: string) => Date.parse(`${day}T00:00:00Z`)

// The API needs a four-digit year for the exclusive upper bound as well.
const reportDate = (value: string | null) => {
  const date = calendarDate(value)
  return date >= '0001-01-01' && date <= '9999-12-30' ? date : ''
}

export function reportFilters(q: URLSearchParams, now: Date) {
  let from = reportDate(q.get('from'))
  let until = reportDate(q.get('until'))
  if (!from && !until) {
    until = isoDay(now.valueOf())
    from = isoDay(now.valueOf() - 29 * DAY)
  }
  from ||= until
  until ||= from
  if (from > until) [from, until] = [until, from]
  if (utcMidnight(until) - utcMidnight(from) > 365 * DAY) {
    from = isoDay(utcMidnight(until) - 365 * DAY)
  }
  return { from, until, category_id: uuidFilter(q.get('category_id')), team_id: uuidFilter(q.get('team_id')) }
}

export function reportQuery(f: ReturnType<typeof reportFilters>) {
  return {
    created_from: `${f.from}T00:00:00Z`,
    created_before: new Date(utcMidnight(f.until) + DAY).toISOString(),
    category_id: f.category_id || undefined,
    team_id: f.team_id || undefined,
  }
}
