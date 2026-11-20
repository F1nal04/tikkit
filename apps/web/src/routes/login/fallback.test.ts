import { render, screen } from '@testing-library/svelte'
import { expect, test, vi } from 'vitest'

vi.mock('#lib/i18n/de.ts', async () => {
  const actual = await vi.importActual<typeof import('#lib/i18n/de.ts')>('#lib/i18n/de.ts')
  return { default: { ...actual.default, signIn: '   ' } }
})

vi.mock('$app/navigation', () => ({ goto: vi.fn() }))

test('a blank German string renders the English heading on the sign-in page', async () => {
  const { default: Login } = await import('./+page.svelte')
  render(Login, { props: { data: { locale: 'de', next: null, user: null }, params: {} } })
  expect(screen.getByRole('heading', { level: 1 }).textContent).toBe('Sign in')
})
