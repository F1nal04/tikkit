import { createClient } from '@tikkit/api-client'
import type { PageLoad } from './$types'
export const load: PageLoad = async ({ fetch, parent }) => {
  await parent()
  return { settings: await createClient({ fetch }).request('get', '/api/admin/settings') }
}
