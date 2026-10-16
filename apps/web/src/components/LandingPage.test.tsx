import { render, screen, waitFor } from '@testing-library/react'
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

// The typed client calls fetch with a Request object.
function requestOf(fetchMock: ReturnType<typeof stubFetch>, index = 0) {
  const [input, init] = fetchMock.mock.calls[index] ?? []
  if (input instanceof Request) return { url: input.url, signal: input.signal }
  return { url: String(input), signal: init?.signal ?? undefined }
}

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
    expect(new URL(requestOf(fetchMock).url, 'http://localhost').pathname).toBe(
      '/api/health',
    )
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

  it('aborts the health check when unmounted', async () => {
    const fetchMock = stubFetch(never)
    const { unmount } = render(<LandingPage />)
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1))

    const { signal } = requestOf(fetchMock)
    expect(signal?.aborted).toBe(false)
    unmount()
    expect(signal?.aborted).toBe(true)
  })
})
