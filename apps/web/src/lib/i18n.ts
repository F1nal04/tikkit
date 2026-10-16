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
  for (const part of (acceptLanguage ?? '').split(',')) {
    const tag = part.split(';')[0].trim().slice(0, 2).toLowerCase()
    if (isLocale(tag)) return tag
  }
  return 'en'
}
