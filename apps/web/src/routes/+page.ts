import type { PageLoad } from './$types'

export const load: PageLoad = async ({ fetch }) => {
  try {
    const response = await fetch('/api/health')
    if (!response.ok) return { healthy: false }
    const data: unknown = await response.json()
    return {
      healthy:
        typeof data === 'object' && data !== null && 'status' in data && data.status === 'ok',
    }
  } catch {
    return { healthy: false }
  }
}
