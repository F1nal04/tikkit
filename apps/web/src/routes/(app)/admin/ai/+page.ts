import { createClient } from '@tikkit/api-client'
import type { PageLoad } from './$types'

export const load: PageLoad = async ({ fetch }) => {
  const { request } = createClient({ fetch })
  const [ai, limits, usage] = await Promise.all([
    request('get', '/api/settings/ai'),
    request('get', '/api/settings/ai/limits'),
    request('get', '/api/settings/ai/usage', { query: { days: 30 } }),
  ])
  return { ai, limits, usage }
}
