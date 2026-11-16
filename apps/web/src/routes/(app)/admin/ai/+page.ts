import { createClient } from '@tikkit/api-client'
import type { PageLoad } from './$types'

export const load: PageLoad = async ({ fetch }) => ({ ai: await createClient({ fetch }).request('get', '/api/settings/ai') })
