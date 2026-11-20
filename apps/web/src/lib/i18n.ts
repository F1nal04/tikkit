import de from './i18n/de'
import en from './i18n/en'

/**
 * German is English with translations laid on top. A missing or blank German string
 * renders the English text, including on pages that read `messages[locale]` directly.
 */
export function mergeCatalog<T>(base: T, overlay: unknown): T {
  if (typeof base === 'string') {
    return (typeof overlay === 'string' && overlay.trim() !== '' ? overlay : base) as T
  }
  if (Array.isArray(base)) {
    const over = Array.isArray(overlay) ? overlay : []
    return base.map((item, index) => mergeCatalog(item, over[index])) as T
  }
  if (base && typeof base === 'object') {
    const over = overlay && typeof overlay === 'object' && !Array.isArray(overlay) ? (overlay as Record<string, unknown>) : {}
    const out: Record<string, unknown> = {}
    for (const key of Object.keys(base as object)) {
      out[key] = mergeCatalog((base as Record<string, unknown>)[key], over[key])
    }
    return out as T
  }
  return (overlay ?? base) as T
}

export const messages = { en, de: mergeCatalog(en, de) }
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

function at(catalog: unknown, key: string): unknown {
  return key.split('.').reduce<unknown>((node, part) => {
    if (Array.isArray(node)) return node[Number(part)]
    if (node && typeof node === 'object' && part in node) return (node as Record<string, unknown>)[part]
    return undefined
  }, catalog)
}

/**
 * Text for `key` in `primary`, or the same key in `fallback` (English) when it is missing or blank.
 * An unknown key is returned as itself only when English has no string either.
 */
export function lookupText(primary: unknown, fallback: unknown, key: string): string {
  const local = at(primary, key)
  if (typeof local === 'string' && local) return local
  const english = at(fallback, key)
  return typeof english === 'string' && english ? english : key
}

/** Active language, then English. Never returns a bare key when English has the string. */
export function translate(locale: Locale, key: string): string {
  return lookupText(messages[locale], messages.en, key)
}

/** Dotted paths of every string, so the two catalogs can be checked for the same shape. */
export function messageKeys(value: unknown, prefix = ''): string[] {
  if (Array.isArray(value)) return value.flatMap((item, index) => messageKeys(item, prefix ? `${prefix}.${index}` : String(index)))
  if (value && typeof value === 'object') {
    return Object.keys(value)
      .sort()
      .flatMap((key) => messageKeys((value as Record<string, unknown>)[key], prefix ? `${prefix}.${key}` : key))
  }
  return prefix ? [prefix] : []
}
