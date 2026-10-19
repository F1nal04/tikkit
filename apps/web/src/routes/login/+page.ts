import { safeNext } from '#lib/redirect.ts'
import type { PageLoad } from './$types'

export const load: PageLoad = ({ url }) => ({ next: safeNext(url.searchParams.get('next')) })
