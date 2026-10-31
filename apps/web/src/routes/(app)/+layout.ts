import { error } from '@sveltejs/kit'
import { canAccess } from '#lib/nav.ts'
import type { LayoutLoad } from './$types'

// Navigation aid only: the API answers 403/404 to anything the role may not do.
// Child loads that await `parent()` stop here, so a denied page never fetches its data.
export const load: LayoutLoad = async ({ parent, url }) => {
  const { user } = await parent()
  if (!user || !canAccess(user.role, url.pathname)) error(403, 'forbidden')
}
