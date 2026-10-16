import { fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, expect, test, vi } from 'vitest'
import Page from './+page.svelte'

const { goto } = vi.hoisted(() => ({ goto: vi.fn() }))
vi.mock('$app/navigation', () => ({ goto }))

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })

afterEach(() => {
  vi.unstubAllGlobals()
  goto.mockReset()
})

async function submit(response: Response) {
  const fetch = vi.fn().mockResolvedValue(response)
  vi.stubGlobal('fetch', fetch)
  render(Page, { props: { data: { locale: 'en', user: null }, params: {} } })
  await fireEvent.input(screen.getByLabelText(/^Username/), { target: { value: 'root' } })
  await fireEvent.input(screen.getByLabelText(/^Email/), { target: { value: 'it@example.com' } })
  await fireEvent.input(screen.getByLabelText(/^Password/), {
    target: { value: 'correct horse battery' },
  })
  await fireEvent.change(screen.getByLabelText(/^Language/), { target: { value: 'de' } })
  await fireEvent.submit(screen.getByRole('button').closest('form')!)
  return fetch
}

test('creates the first admin and navigates home', async () => {
  const fetch = await submit(json(200, { id: '1', username: 'root' }))
  const [url, init] = fetch.mock.calls[0]
  expect(url).toBe('/api/setup')
  expect(init.headers['x-requested-with']).toBe('tikkit')
  expect(JSON.parse(init.body)).toEqual({
    username: 'root',
    email: 'it@example.com',
    password: 'correct horse battery',
    language: 'de',
  })
  await waitFor(() => expect(goto).toHaveBeenCalledWith('/', { invalidateAll: true }))
})

test('shows field errors linked to their inputs', async () => {
  await submit(
    json(422, {
      error: {
        code: 'validation_failed',
        message: 'x',
        details: { fields: { password: ['too_common'] } },
      },
      request_id: 'r',
    }),
  )
  await screen.findByRole('alert')
  const input = screen.getByLabelText(/^Password/)
  expect(input.getAttribute('aria-invalid')).toBe('true')
  expect(input.getAttribute('aria-describedby')).toBe('password-hint password-error')
  expect(document.getElementById('password-error')?.textContent).toBe(
    'This password is too easy to guess.',
  )
})

test('reports that setup is already complete', async () => {
  await submit(
    json(409, { error: { code: 'setup_already_completed', message: 'x' }, request_id: 'r' }),
  )
  expect((await screen.findByRole('alert')).textContent).toContain('already been completed')
  expect(goto).not.toHaveBeenCalled()
})
