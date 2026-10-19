import { createClient } from '@tikkit/api-client'
import type { PageLoad } from './$types'

export const load: PageLoad = async ({ fetch, parent }) => {
  const { user } = await parent()
  return { smtp: await createClient({ fetch }).request('get', '/api/settings/smtp'), accountEmail: user?.email ?? '' }
}
