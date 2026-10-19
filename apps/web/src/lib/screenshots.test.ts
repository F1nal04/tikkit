import { fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, expect, test, vi } from 'vitest'
import ScreenshotPicker from './ScreenshotPicker.svelte'

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })

const shot = (id: string, filename: string) => ({
  id,
  filename,
  content_type: 'image/png',
  byte_size: 10,
  width: 4,
  height: 3,
  created_at: '2026-10-19T00:00:00Z',
})

const invalid = (field: string, code: string) =>
  json(422, {
    error: { code: 'validation_failed', message: '', details: { fields: { [field]: [code] } } },
    request_id: 'r',
  })

afterEach(() => vi.unstubAllGlobals())

async function choose(...files: File[]) {
  const input = screen.getByLabelText(/screenshot/i) as HTMLInputElement
  Object.defineProperty(input, 'files', { value: files, configurable: true })
  await fireEvent.change(input)
}

test('uploads raw images, previews them, reports failures per file, and removes', async () => {
  const fetch = vi
    .fn()
    .mockResolvedValueOnce(json(201, shot('s1', 'shot.png')))
    .mockResolvedValueOnce(invalid('file', 'corrupt'))
  vi.stubGlobal('fetch', fetch)
  render(ScreenshotPicker, { props: { locale: 'en' } })
  const ok = new File([new Uint8Array([1])], 'shot.png', { type: 'image/png' })
  const broken = new File([new Uint8Array([2])], 'broken.png', { type: 'image/png' })
  const svg = new File(['<svg/>'], 'x.svg', { type: 'image/svg+xml' })
  await choose(ok, broken, svg)

  await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('broken.png'))
  expect(screen.getByAltText('shot.png').getAttribute('src')).toBe('/api/screenshots/s1/preview')
  const [url, init] = fetch.mock.calls[0]
  expect(url).toBe('/api/screenshots?filename=shot.png')
  expect(init.body).toBe(ok)
  expect(init.headers['content-type']).toBe('image/png')
  expect(init.headers['x-requested-with']).toBe('tikkit')
  expect(fetch).toHaveBeenCalledTimes(2) // the SVG is refused before any request
  const alert = screen.getByRole('alert').textContent ?? ''
  expect(alert).toContain('broken.png: This image is damaged or cannot be read.')
  expect(alert).toContain('x.svg: Only PNG, JPEG, and WebP images can be attached.')

  await fireEvent.click(screen.getByRole('button', { name: 'Remove: shot.png' }))
  expect(screen.queryByAltText('shot.png')).toBeNull()
})

test('resumes draft IDs with their metadata and speaks German', async () => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(json(200, shot('s9', 'bild.webp'))))
  render(ScreenshotPicker, { props: { locale: 'de', ids: ['s9'] } })
  await waitFor(() => expect(screen.getByAltText('bild.webp')).toBeTruthy())
  expect(screen.getByRole('button', { name: 'Entfernen: bild.webp' })).toBeTruthy()
  expect(screen.getByText(/bis zu 10 MiB/)).toBeTruthy()
})

test('maps server limits to clear messages', async () => {
  vi.stubGlobal(
    'fetch',
    vi
      .fn()
      .mockResolvedValueOnce(json(413, { error: { code: 'payload_too_large', message: '' }, request_id: 'r' }))
      .mockResolvedValueOnce(invalid('filename', 'type_mismatch'))
      .mockRejectedValueOnce(new TypeError('offline')),
  )
  render(ScreenshotPicker, { props: { locale: 'en' } })
  const file = (name: string) => new File([new Uint8Array([1])], name, { type: 'image/png' })
  await choose(file('a.png'), file('b.jpg'), file('c.png'))
  await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('c.png'))
  const alert = screen.getByRole('alert').textContent ?? ''
  expect(alert).toContain('a.png: This file is larger than 10 MiB.')
  expect(alert).toContain('b.jpg: The file type does not match its content or file name.')
  expect(alert).toContain('c.png: The service is unavailable.')
})
