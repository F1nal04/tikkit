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
