import { createClient } from '@tikkit/api-client'
import type { PageLoad } from './$types'

export const load: PageLoad = async ({ fetch, parent }) => {
  await parent()
  // Without preferences (API down) the profile still works; only that form is hidden.
  const prefs = await createClient({ fetch }).request('get', '/api/notification-preferences').catch(() => null)
  return prefs ? { prefs } : {}
}
