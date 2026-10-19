import { error } from '@sveltejs/kit'
import { createClient } from '@tikkit/api-client'
import type { PageLoad } from './$types'

export const load: PageLoad = async ({ fetch, parent }) => {
  const { user } = await parent()
  // Navigation aid only; the API answers 403 to non-Admins.
  if (user?.role !== 'admin') error(403, 'forbidden')
  const api = createClient({ fetch })
  const smtp = await api.request('get', '/api/settings/smtp')
  return { smtp, accountEmail: user.email }
}
