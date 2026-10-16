import { createClient } from '@tikkit/api-client'
import type { PageLoad } from './$types'

export const load: PageLoad = async ({ fetch }) => {
  const health = await createClient({ fetch })
    .request('get', '/api/health')
    .catch(() => null)
  return { healthy: health?.status === 'ok' }
}
