import { render, screen } from '@testing-library/svelte'
import { expect, test } from 'vitest'
import { pageLocale, resolveLocale } from '#lib/i18n.ts'
import Page from './+page.svelte'

test('shows API health in the selected language', () => {
  render(Page, { props: { data: { locale: 'de', user: null, healthy: false }, params: {} } })
  expect(screen.getByRole('status').textContent).toBe('Rust-API nicht erreichbar')
  expect(screen.getByText('Ein sauberer Ausgangspunkt.')).toBeTruthy()
})

test('shows a healthy API in English', () => {
  render(Page, { props: { data: { locale: 'en', user: null, healthy: true }, params: {} } })
  expect(screen.getByRole('status').textContent).toBe('Rust API connected')
})

test('resolves locale from query, cookie, then Accept-Language', () => {
  expect(resolveLocale('de', 'en', 'en')).toBe('de')
  expect(resolveLocale('fr', 'de', 'en')).toBe('de')
  expect(resolveLocale(null, undefined, 'fr-FR,de-DE;q=0.8')).toBe('de')
  expect(resolveLocale(null, undefined, null)).toBe('en')
  expect(resolveLocale(null, undefined, 'en;q=0.1, de;q=0.9')).toBe('de')
  expect(resolveLocale(null, undefined, 'de;q=0, en')).toBe('en')
})

test("signed in, the account's language wins unless ?lang= chose one", () => {
  expect(pageLocale('en', null, 'de')).toBe('de')
  expect(pageLocale('en', 'en', 'de')).toBe('en')
  expect(pageLocale('de', 'fr', 'en')).toBe('en')
  expect(pageLocale('de', null, undefined)).toBe('de')
})
