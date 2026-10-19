import de from './i18n/de'
import en from './i18n/en'

export const messages = { en, de }
export type Locale = keyof typeof messages

const isLocale = (value: string | null | undefined): value is Locale =>
  value === 'en' || value === 'de'

/** Explicit `?lang=`, then the `lang` cookie, then Accept-Language; English fallback. */
export function resolveLocale(
  query: string | null,
  cookie: string | undefined,
  acceptLanguage: string | null,
): Locale {
  if (isLocale(query)) return query
  if (isLocale(cookie)) return cookie
  const ranked = (acceptLanguage ?? '')
    .split(',')
    .map((part) => {
      const [tag, ...params] = part.split(';')
      const q = params.map((p) => p.trim()).find((p) => p.startsWith('q='))
      return { tag: tag.trim().slice(0, 2).toLowerCase(), q: q ? Number(q.slice(2)) || 0 : 1 }
    })
    .filter(({ q }) => q > 0)
    .sort((a, b) => b.q - a.q)
  return ranked.map(({ tag }) => tag).find(isLocale) ?? 'en'
}

/** Signed in, the account's language applies unless this request picked one with `?lang=`. */
export function pageLocale(resolved: Locale, query: string | null, account: Locale | undefined): Locale {
  return account && query !== resolved ? account : resolved
}

type Messages = (typeof messages)[Locale]

/** Localized text for a failed API call, chosen by its stable error code. */
export function errorText(t: Messages, error: unknown): string {
  const code = (error as { code?: unknown } | null)?.code
  return typeof code === 'string' && code in t.errors
    ? t.errors[code as keyof Messages['errors']]
    : t.errors.generic
}

/** Localized text for a field error code from `validation_failed`. */
export function fieldText(t: Messages, code: string | undefined): string | undefined {
  if (!code) return undefined
  return code in t.fieldErrors ? t.fieldErrors[code as keyof Messages['fieldErrors']] : t.fieldErrors.invalid
}
