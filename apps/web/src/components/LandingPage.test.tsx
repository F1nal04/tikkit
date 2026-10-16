import { render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { LandingPage } from './LandingPage'

function stubFetch(impl: () => Promise<Response>) {
  const fetchMock = vi.fn<typeof fetch>(impl)
  vi.stubGlobal('fetch', fetchMock)
  return fetchMock
}

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  })
}

const never = () => new Promise<Response>(() => {})

describe('LandingPage', () => {
  it('shows a pending status while the health check is in flight', () => {
    stubFetch(never)
    render(<LandingPage />)

    expect(screen.getByRole('heading', { name: 'Tikkit' })).toBeInTheDocument()
    expect(screen.getByRole('status')).toHaveTextContent('Checking API…')
  })

  it('reports the API as connected when /api/health returns ok', async () => {
    const fetchMock = stubFetch(async () => jsonResponse({ status: 'ok' }))
    render(<LandingPage />)

    expect(await screen.findByText('Rust API connected')).toBeInTheDocument()
    expect(screen.getByRole('status')).toHaveAttribute(
      'data-health',
      'connected',
    )
    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(fetchMock.mock.calls[0]?.[0]).toBe('/api/health')
  })

  it('reports the API as unavailable on a non-2xx response', async () => {
    stubFetch(async () =>
      jsonResponse({ error: { code: 'internal_error' } }, 500),
    )
    render(<LandingPage />)

    expect(await screen.findByText('Rust API unavailable')).toBeInTheDocument()
  })

  it('reports the API as unavailable when the body is not a healthy status', async () => {
    stubFetch(async () => jsonResponse({ status: 'degraded' }))
    render(<LandingPage />)

    expect(await screen.findByText('Rust API unavailable')).toBeInTheDocument()
  })

  it('reports the API as unavailable when the request fails', async () => {
    stubFetch(async () => {
      throw new TypeError('Failed to fetch')
    })
    render(<LandingPage />)

    expect(await screen.findByText('Rust API unavailable')).toBeInTheDocument()
  })

  it('aborts the health check when unmounted', () => {
    const fetchMock = stubFetch(never)
    const { unmount } = render(<LandingPage />)

    const signal = fetchMock.mock.calls[0]?.[1]?.signal
    expect(signal?.aborted).toBe(false)
    unmount()
    expect(signal?.aborted).toBe(true)
  })
})
