import { ApiError, createClient, type Language } from '@tikkit/api-client'
import { session } from './session.svelte.ts'

/** The API client for browser actions: a 401 flags the expired session, a success clears it. */
export function api(language: Language) {
  const client = createClient({ language })
  const request = (async (...args: Parameters<typeof client.request>) => {
    try {
      const result = await client.request(...args)
      session.expired = false
      return result
    } catch (e) {
      if (e instanceof ApiError && e.code === 'unauthorized') session.expired = true
      throw e
    }
  }) as typeof client.request
  return { request }
}
