import { useEffect, useState } from 'react'

export type ApiHealth = 'checking' | 'connected' | 'unavailable'

const statusText: Record<ApiHealth, string> = {
  checking: 'Checking API…',
  connected: 'Rust API connected',
  unavailable: 'Rust API unavailable',
}

function isHealthy(data: unknown): boolean {
  return (
    typeof data === 'object' &&
    data !== null &&
    'status' in data &&
    data.status === 'ok'
  )
}

export function useApiHealth(): ApiHealth {
  const [health, setHealth] = useState<ApiHealth>('checking')

  useEffect(() => {
    const controller = new AbortController()

    async function checkHealth() {
      try {
        const response = await fetch('/api/health', {
          signal: controller.signal,
        })
        if (!response.ok) throw new Error('Health check failed')
        const data: unknown = await response.json()
        setHealth(isHealthy(data) ? 'connected' : 'unavailable')
      } catch {
        if (!controller.signal.aborted) setHealth('unavailable')
      }
    }

    void checkHealth()
    return () => controller.abort()
  }, [])

  return health
}

export function LandingPage() {
  const health = useApiHealth()

  return (
    <main>
      <p className="eyebrow">TanStack Start + Rust</p>
      <h1>Tikkit</h1>
      <p>A clean starting point.</p>
      <p className="status" role="status" data-health={health}>
        {statusText[health]}
      </p>
    </main>
  )
}
