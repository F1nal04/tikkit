import { expect, test } from 'vitest'
import { errorText, lookupText, messageKeys, messages, pageLocale, resolveLocale } from '#lib/i18n.ts'

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

test('English and German expose the same message keys', () => {
  expect(messageKeys(messages.de)).toEqual(messageKeys(messages.en))
})

test('a missing translation renders the English text instead of the key', () => {
  const english = { signIn: 'Sign in', nested: { title: 'Hello' } }
  expect(lookupText({ signIn: 'Anmelden' }, english, 'signIn')).toBe('Anmelden')
  expect(lookupText({ signIn: '' }, english, 'signIn')).toBe('Sign in')
  expect(lookupText({}, english, 'nested.title')).toBe('Hello')
  expect(lookupText({}, english, 'missing.key')).toBe('missing.key')
  expect(lookupText(messages.de, messages.en, 'signIn')).toBe(messages.de.signIn)
})

test('dates and numbers follow the active locale', () => {
  const stamp = (locale: string) =>
    new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'short', timeZone: 'UTC' }).format(new Date('2026-10-20T08:00:00Z'))
  expect(stamp('en')).toContain('Oct')
  expect(stamp('de')).toContain('20.10.2026')
  expect(stamp('en')).not.toBe(stamp('de'))
  expect(new Intl.NumberFormat('en').format(1234.5)).toBe('1,234.5')
  expect(new Intl.NumberFormat('de').format(1234.5)).toBe('1.234,5')
})

test('a refused cross-site or wrong-address request tells the user to open the configured address', () => {
  expect(errorText(messages.en, { code: 'csrf_rejected' })).toContain('configured address')
  expect(errorText(messages.de, { code: 'csrf_rejected' })).toContain('eingerichteten Adresse')
  expect(errorText(messages.en, { code: 'request_timeout' })).toContain('took too long')
})
