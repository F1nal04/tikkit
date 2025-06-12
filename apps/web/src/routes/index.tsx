import { createFileRoute } from '@tanstack/react-router'
import { useEffect, useState } from 'react'

export const Route = createFileRoute('/')({ component: Home })

function Home() {
  const [status, setStatus] = useState('Checking API…')

  useEffect(() => {
    const controller = new AbortController()

    async function checkHealth() {
      try {
        const response = await fetch('/api/health', {
          signal: controller.signal,
        })
        if (!response.ok) throw new Error('Health check failed')
        const data: unknown = await response.json()
        const healthy =
          typeof data === 'object' &&
          data !== null &&
          'status' in data &&
          data.status === 'ok'
        setStatus(healthy ? 'Rust API connected' : 'Rust API unavailable')
      } catch {
        if (!controller.signal.aborted) setStatus('Rust API unavailable')
      }
    }

    void checkHealth()
    return () => controller.abort()
  }, [])

  return (
    <main>
      <p className="eyebrow">TanStack Start + Rust</p>
      <h1>Tikkit</h1>
      <p>A clean starting point.</p>
      <p className="status" role="status">{status}</p>
    </main>
  )
}
