import { fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, expect, test, vi } from 'vitest'
import Page from './+page.svelte'
import { fieldBody } from './form.ts'

const { invalidateAll } = vi.hoisted(() => ({ invalidateAll: vi.fn() }))
vi.mock('$app/navigation', () => ({ invalidateAll }))

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })

const catalog = {
  categories: [
    { id: 'c1', name: 'Software', skip_ai: false, archived: false },
    { id: 'c2', name: 'Access', skip_ai: true, archived: true },
  ],
  priorities: [{ id: 'p1', name: 'Normal', archived: false }],
  fields: [
    {
      id: 'f1',
      label: 'Program',
      guidance: '',
      kind: 'select' as const,
      required: true,
      allow_other: true,
      category_ids: ['c1'],
      choices: [{ id: 'k1', label: 'Word', archived: false }],
      archived: false,
    },
  ],
}

afterEach(() => {
  vi.unstubAllGlobals()
  invalidateAll.mockReset()
})

function show(locale: 'en' | 'de', response = json(201, {})) {
  const fetch = vi.fn().mockResolvedValue(response)
  vi.stubGlobal('fetch', fetch)
  render(Page, { props: { data: { locale, user: null, catalog }, params: {} } })
  return fetch
}

test('builds the complete choice list from the field form', () => {
  const form = new FormData()
  form.set('label', 'Program')
  form.append('category_ids', 'c1')
  form.set('allow_other', 'on')
  form.append('choice_id', 'k1')
  form.append('choice_label', 'MS Word')
  form.append('choice_id', 'k2')
  form.append('choice_label', 'Excel')
  form.append('choice_archived', 'k2')
  form.set('new_choices', ' Outlook \n\nTeams')
  expect(fieldBody(form)).toEqual({
    label: 'Program',
    guidance: '',
    required: false,
    allow_other: true,
    category_ids: ['c1'],
    choices: [
      { id: 'k1', label: 'MS Word', archived: false },
      { id: 'k2', label: 'Excel', archived: true },
      { label: 'Outlook' },
      { label: 'Teams' },
    ],
  })
})

test('creates a category that skips AI automatically', async () => {
  const fetch = show('en')
  expect(screen.getByRole('button', { name: 'Restore' })).toBeTruthy()
  const create = screen.getAllByRole('button', { name: 'Add' })[0].closest('form')!
  const name = create.querySelector<HTMLInputElement>('input[name=name]')!
  await fireEvent.input(name, { target: { value: 'Hardware' } })
  await fireEvent.click(create.querySelector<HTMLInputElement>('input[name=skip_ai]')!)
  await fireEvent.submit(create)
  const [url, init] = fetch.mock.calls[0]
  expect([url, init.method]).toEqual(['/api/categories', 'POST'])
  expect(JSON.parse(init.body)).toEqual({ name: 'Hardware', skip_ai: true })
  await waitFor(() => expect(invalidateAll).toHaveBeenCalled())
})

test('reorders with the complete list and shows errors in German', async () => {
  const fetch = show('de', json(409, { error: { code: 'conflict', message: 'x' }, request_id: 'r' }))
  expect(screen.getByRole('heading', { level: 1 }).textContent).toBe('Ticketkatalog')
  await fireEvent.click(screen.getByRole('button', { name: 'Nach unten: Software' }))
  const [url, init] = fetch.mock.calls[0]
  expect([url, init.method]).toEqual(['/api/categories/order', 'PUT'])
  expect(JSON.parse(init.body)).toEqual({ ids: ['c2', 'c1'] })
  expect((await screen.findByRole('alert')).textContent).toContain('inzwischen geändert')
})
