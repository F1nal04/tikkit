import { createClient, type Schemas } from '@tikkit/api-client'
import type { PageLoad } from './$types'

const OUTCOMES = ['solved', 'not_solved', 'skipped', 'failed', 'cancelled', 'awaiting_decision', 'processing'] as const satisfies readonly Schemas['AiRecordOutcome'][]

export const load: PageLoad = async ({ fetch, url }) => {
  const { request } = createClient({ fetch })
  const outcome = OUTCOMES.find((o) => o === url.searchParams.get('outcome'))
  const cursor = url.searchParams.get('cursor') || undefined
  const [stats, records] = await Promise.all([
    request('get', '/api/admin/ai/outcomes', { query: { days: 30 } }),
    request('get', '/api/admin/ai/records', { query: { outcome, cursor } }),
  ])
  return { stats, records, outcome: outcome ?? '', cursor: cursor ?? '', outcomes: OUTCOMES }
}
