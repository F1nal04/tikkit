import { error } from '@sveltejs/kit'
import { createClient } from '@tikkit/api-client'
import type { PageLoad } from './$types'

export const load: PageLoad = async ({ fetch, parent }) => {
  const { user } = await parent()
  // Navigation aid only; the API answers 403 to non-Admins.
  if (user?.role !== 'admin') error(403, 'forbidden')
  return { catalog: await createClient({ fetch }).request('get', '/api/catalog/all') }
}
