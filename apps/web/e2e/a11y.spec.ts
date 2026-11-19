import { readdirSync, statSync } from 'node:fs'
import { join, relative } from 'node:path'
import AxeBuilder from '@axe-core/playwright'
import { expect, type Page, test } from '@playwright/test'
import { WEB } from '../playwright.config'
import { proxyApi, signIn } from './support'

/**
 * WCAG 2.2 A/AA tags axe-core implements. `wcag22aa` is the 2.2 AA rule (target size);
 * 2.0 and 2.1 A/AA tags cover the rest of the level that axe ships.
 * A new exclusion needs a reason here and in docs/accessibility.md.
 */
const TAGS = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa']
const EXCLUSIONS: Record<string, string> = {}

const VIEWPORTS = [
  { name: 'mobile', width: 390, height: 844 },
  { name: 'tablet', width: 768, height: 1024 },
  { name: 'desktop', width: 1280, height: 800 },
] as const

const PNG = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
  'base64',
)
const GENERAL = '30000000-0000-4000-8000-000000000001'
const DESK = '30000000-0000-4000-8000-000000000101'
const CLAIMED = '30000000-0000-4000-8000-000000000102'
const SUBMISSION = '60000000-0000-4000-8000-000000000301'

/** Every `+page.svelte`, with route groups removed, so a new page fails the scan until it is listed. */
export function discoverPageRoutes(root = join(import.meta.dirname, '../src/routes')): string[] {
  const found: string[] = []
  const walk = (dir: string) => {
    for (const name of readdirSync(dir)) {
      const full = join(dir, name)
      if (statSync(full).isDirectory()) walk(full)
      else if (name === '+page.svelte') {
        const parts = relative(root, dir).split(/[\\/]/).filter((part) => part && !part.startsWith('('))
        found.push(parts.length ? `/${parts.join('/')}` : '/')
      }
    }
  }
  walk(root)
  return found.sort()
}

const COVERED = [
  '/',
  '/admin',
  '/admin/accounts',
  '/admin/ai',
  '/admin/ai/records',
  '/admin/audit',
  '/admin/catalog',
  '/admin/email',
  '/admin/reports',
  '/admin/retention',
  '/admin/settings',
  '/admin/submissions',
  '/admin/submissions/[id]',
  '/admin/teams',
  '/forgot-password',
  '/invite/[token]',
  '/login',
  '/notifications',
  '/profile',
  '/queue',
  '/queue/[id]',
  '/reset-password/[token]',
  '/setup',
  '/team',
  '/tickets/[id]',
  '/tickets/new',
]

test('a new page route fails until the accessibility scan visits it', () => {
  expect(discoverPageRoutes()).toEqual([...COVERED].sort())
})

type Lang = 'en' | 'de'

function withLang(path: string, lang: Lang) {
  const url = new URL(path, 'http://localhost')
  url.searchParams.set('lang', lang)
  return `${url.pathname}${url.search}`
}

async function call(page: Page, path: string, body?: unknown, method = 'POST') {
  return page.evaluate(async ({ path, body, method }) => {
    const response = await fetch(path, {
      method,
      headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' },
      body: JSON.stringify(body ?? {}),
    })
    const text = await response.text()
    try {
      return JSON.parse(text) as Record<string, unknown>
    } catch {
      return {}
    }
  }, { path, body, method })
}

function formatViolation(violation: { id: string; impact?: string | null; help: string; nodes: { target: unknown; failureSummary?: string }[] }) {
  const nodes = violation.nodes.slice(0, 4).map((node) => {
    const target = (Array.isArray(node.target) ? node.target : [node.target])
      .map((part) => (typeof part === 'string' ? part : JSON.stringify(part)))
      .join(' ')
    return `    ${target} — ${(node.failureSummary ?? '').split('\n')[0]}`
  }).join('\n')
  return `${violation.id} (${violation.impact ?? 'unknown'}): ${violation.help}\n${nodes}`
}

async function inspect(page: Page, lang: Lang, label: string) {
  const problems: string[] = []
  try {
    await expect(page.getByRole('heading', { level: 1 })).toBeVisible({ timeout: 15_000 })
  } catch (error) {
    const text = await page.locator('body').innerText().catch(() => '')
    return [`${label}: page did not render\n${text.slice(0, 400)}\n${error}`]
  }
  const width = page.viewportSize()?.width ?? 1280
  if (width < 768) {
    const menu = page.getByRole('button', { name: /^(Menu|Menü)$/ })
    try {
      await menu.waitFor({ state: 'visible', timeout: 3000 })
      if ((await menu.getAttribute('aria-expanded')) !== 'true') await menu.click({ timeout: 3000 })
    } catch {
      // Public pages have no navigation menu.
    }
  }
  await expect(page.locator('html')).toHaveAttribute('lang', lang)
  const fits = await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)
  if (!fits) problems.push(`${label}: horizontal overflow`)
  const results = await new AxeBuilder({ page }).withTags(TAGS).analyze()
  for (const violation of results.violations) {
    if (violation.id in EXCLUSIONS) continue
    problems.push(`${label}: ${formatViolation(violation)}`)
  }
  return problems
}

async function open(page: Page, lang: Lang, path: string, seen: Set<string>) {
  const route = path
    .replace(/^\/invite\/.*/, '/invite/[token]')
    .replace(/^\/reset-password\/.*/, '/reset-password/[token]')
    .replace(/^\/tickets\/(?!new$).*/, '/tickets/[id]')
    .replace(/^\/queue\/.*/, '/queue/[id]')
    .replace(/^\/admin\/submissions\/.*/, '/admin/submissions/[id]')
  seen.add(route)
  await page.goto(withLang(path, lang))
  return inspect(page, lang, `${lang} ${route} (${path})`)
}

async function signInAs(page: Page, username: string, lang: Lang) {
  // A ?lang= visit stores a cookie, which would relabel the sign-in form.
  await page.context().clearCookies({ name: 'lang' })
  await signIn(page, username)
  if (lang === 'de') {
    // Signed in, the account language wins on the next navigation. Set it, or ?lang= only lasts one request.
    await call(page, '/api/auth/me', { language: 'de' }, 'PATCH')
    await page.context().addCookies([{ name: 'lang', value: 'de', url: WEB }])
  }
}

async function signOut(page: Page) {
  const menu = page.getByRole('button', { name: /^(Menu|Menü)$/ })
  if (await menu.isVisible()) {
    if ((await menu.getAttribute('aria-expanded')) !== 'true') await menu.click()
  }
  await page.getByRole('button', { name: /^(Sign out|Abmelden)$/ }).click()
  await expect(page).toHaveURL(/\/login/)
}

async function fillAndSubmit(page: Page, lang: Lang) {
  const de = lang === 'de'
  await page.goto(withLang('/tickets/new', lang))
  await page.waitForLoadState('networkidle')
  await page.getByLabel(de ? 'Kategorie' : 'Category').selectOption({ label: 'Software' })
  await page.getByLabel('Program').selectOption({ label: 'Word' })
  await page.getByLabel(de ? 'Betreff' : 'Subject').fill('Word crashes')
  await page.getByLabel(de ? 'Beschreibung' : 'Description').fill('It closes on startup.')
  await page.getByLabel(de ? 'Priorität' : 'Priority', { exact: true }).selectOption({ label: 'High' })
  await page.getByLabel(de ? 'Screenshots hinzufügen' : 'Add screenshots').setInputFiles({
    name: 'error.png', mimeType: 'image/png', buffer: PNG,
  })
  await expect(page.getByRole('img', { name: 'error.png' })).toBeVisible()
  await page.getByRole('button', { name: de ? 'Anfrage absenden' : 'Submit request' }).click()
  await expect(page).toHaveURL(/\/tickets\/[0-9a-f-]{36}/, { timeout: 15_000 })
  return new URL(page.url()).pathname.split('/').pop()!
}

async function audit(page: Page, lang: Lang) {
  const problems: string[] = []
  const seen = new Set<string>()
  await proxyApi(page)

  for (const path of ['/login', '/forgot-password', '/reset-password/example-token', '/invite/example-token']) {
    problems.push(...await open(page, lang, path, seen))
  }
  await page.context().addCookies([{ name: 'tikkit_setup', value: '1', url: WEB }])
  problems.push(...await open(page, lang, '/setup', seen))
  await expect(page.getByRole('heading', { level: 1 })).toHaveText(lang === 'de' ? 'Tikkit einrichten' : 'Set up Tikkit')
  await page.context().clearCookies({ name: 'tikkit_setup' })

  const shared = ['/', '/tickets/new', '/profile', '/notifications']
  await signInAs(page, 'requester', lang)
  await call(page, '/api/test/ai', { ai_assistance: true })
  // The form, with the conditional program field and a screenshot, before it is sent.
  await page.goto(withLang('/tickets/new', lang))
  await page.waitForLoadState('networkidle')
  seen.add('/tickets/new')
  const de = lang === 'de'
  await page.getByLabel(de ? 'Kategorie' : 'Category').selectOption({ label: 'Software' })
  await page.getByLabel('Program').selectOption({ label: 'Word' })
  await page.getByLabel(de ? 'Screenshots hinzufügen' : 'Add screenshots').setInputFiles({
    name: 'error.png', mimeType: 'image/png', buffer: PNG,
  })
  await expect(page.getByRole('img', { name: 'error.png' })).toBeVisible()
  problems.push(...await inspect(page, lang, `${lang} /tickets/new`))

  const processing = await fillAndSubmit(page, lang)
  seen.add('/tickets/[id]')
  problems.push(...await inspect(page, lang, `${lang} /tickets/[id] processing`))
  await expect(page.locator('.ai-step')).toHaveAttribute('aria-busy', 'true')

  await call(page, '/api/test/ai-result', { id: processing, outcome: 'suggestion' })
  await page.goto(withLang(`/tickets/${processing}`, lang))
  problems.push(...await inspect(page, lang, `${lang} /tickets/[id] suggestion`))
  // The account language is English, so the stored suggestion keeps `lang=en` when the UI is German.
  await expect(page.getByTestId('ai-suggestion')).toHaveAttribute('lang', lang)

  await call(page, `/api/tickets/${processing}/actions`, { action: 'ai_solved' })
  await page.goto(withLang(`/tickets/${processing}`, lang))
  problems.push(...await inspect(page, lang, `${lang} /tickets/[id] solved`))

  const handed = await fillAndSubmit(page, lang)
  await call(page, '/api/test/ai-result', { id: handed, outcome: 'timeout' })
  await page.goto(withLang(`/tickets/${handed}`, lang))
  problems.push(...await inspect(page, lang, `${lang} /tickets/[id] handed off`))

  const failed = await fillAndSubmit(page, lang)
  await call(page, '/api/test/ai-result', { id: failed, outcome: 'ai_failed' })
  await page.goto(withLang(`/tickets/${failed}`, lang))
  problems.push(...await inspect(page, lang, `${lang} /tickets/[id] failed`))

  const history = (await call(page, '/api/test/history', {})) as { pending: string }
  await page.goto(withLang('/', lang))
  seen.add('/')
  problems.push(...await inspect(page, lang, `${lang} /`))
  await page.goto(withLang(`/tickets/${history.pending}`, lang))
  problems.push(...await inspect(page, lang, `${lang} /tickets/[id] requester detail`))
  const detail = await page.getByRole('main').innerText()
  if (lang === 'de') expect(detail).toContain('1.10.2026')
  else expect(detail).toContain('10/1/2026')
  for (const path of ['/profile', '/notifications']) problems.push(...await open(page, lang, path, seen))
  await signOut(page)

  await signInAs(page, 'operator', lang)
  for (const path of [...shared, '/queue', `/queue/${DESK}`, `/queue/${CLAIMED}`]) problems.push(...await open(page, lang, path, seen))
  await signOut(page)

  await signInAs(page, 'specialist', lang)
  for (const path of [...shared, '/queue', `/queue/${GENERAL}`]) problems.push(...await open(page, lang, path, seen))
  await signOut(page)

  await signInAs(page, 'lead', lang)
  for (const path of [...shared, '/queue', '/team', `/queue/${DESK}`]) problems.push(...await open(page, lang, path, seen))
  await signOut(page)

  await signInAs(page, 'admin', lang)
  const admin = [
    ...shared, '/queue', `/queue/${GENERAL}`, '/admin', '/admin/accounts', '/admin/teams', '/admin/settings',
    '/admin/email', '/admin/ai', '/admin/ai/records', '/admin/catalog', '/admin/submissions',
    `/admin/submissions/${SUBMISSION}`, '/admin/audit', '/admin/reports', '/admin/retention',
  ]
  for (const path of admin) problems.push(...await open(page, lang, path, seen))
  await page.goto(withLang('/admin/ai/records', lang))
  const records = await page.getByRole('main').innerText()
  if (lang === 'de') expect(records).toContain('20.10.2026')
  else expect(records).toContain('Oct 20, 2026')

  const missing = COVERED.filter((route) => !seen.has(route))
  if (missing.length) problems.push(`not scanned: ${missing.join(', ')}`)
  return problems
}

for (const viewport of VIEWPORTS) {
  for (const lang of ['en', 'de'] as const) {
    test(`${lang} ${viewport.name} has no WCAG 2.2 A/AA violations`, async ({ page }) => {
      test.setTimeout(180_000)
      page.setDefaultTimeout(20_000)
      await page.setViewportSize({ width: viewport.width, height: viewport.height })
      const problems = await audit(page, lang)
      expect(problems, problems.join('\n\n')).toEqual([])
    })
  }
}

test('a stored suggestion keeps its own language when the interface language changes', async ({ page }) => {
  await signIn(page, 'requester')
  await call(page, '/api/test/ai', { ai_assistance: true })
  await page.goto('/tickets/new')
  await page.getByLabel('Category').selectOption({ label: 'Software' })
  await page.getByLabel('Program').selectOption({ label: 'Word' })
  await page.getByLabel('Subject').fill('Word crashes')
  await page.getByLabel('Description').fill('It closes on startup.')
  await page.getByLabel('Priority', { exact: true }).selectOption({ label: 'High' })
  await page.getByRole('button', { name: 'Submit request' }).click()
  await expect(page).toHaveURL(/\/tickets\/[0-9a-f-]{36}/)
  const id = new URL(page.url()).pathname.split('/').pop()!
  await call(page, '/api/test/ai-result', { id, outcome: 'suggestion' })
  await call(page, '/api/auth/me', { language: 'de' }, 'PATCH')
  await page.goto(`/tickets/${id}`)
  await expect(page.locator('html')).toHaveAttribute('lang', 'de')
  await expect(page.getByTestId('ai-suggestion')).toHaveAttribute('lang', 'en')
  await expect(page.getByText(/genau so angezeigt/)).toBeVisible()
})

test('a queue change is announced in a polite live region', async ({ page }) => {
  await signIn(page, 'operator')
  await page.goto('/queue')
  await expect(page.getByTestId('queue-announcement')).toHaveText('')
  const dispatched = await page.evaluate(async () => {
    const id = '30000000-0000-4000-8000-000000000001'
    const ticket = await fetch(`/api/tickets/${id}`).then((response) => response.json())
    const response = await fetch(`/api/tickets/${id}/assignment`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' },
      body: JSON.stringify({
        action: 'dispatch',
        team_id: '50000000-0000-4000-8000-000000000002',
        expected_updated_at: ticket.updated_at,
      }),
    })
    return response.status
  })
  expect(dispatched).toBe(200)
  await page.evaluate(() => document.dispatchEvent(new Event('visibilitychange')))
  await expect(page.getByTestId('queue-announcement')).toHaveText('The queue was updated.')
})

test('text at 200% stays on the page in both languages', async ({ page }) => {
  await proxyApi(page)
  for (const lang of ['en', 'de'] as const) {
    await page.setViewportSize({ width: 1280, height: 800 })
    await page.goto(withLang('/login', lang))
    await page.evaluate(() => { document.documentElement.style.fontSize = '32px' })
    await expect(page.getByRole('heading', { level: 1 })).toBeVisible()
    await expect(page.getByLabel(lang === 'de' ? 'Benutzername' : 'Username')).toBeVisible()
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  }
})

test('a phone-width page does not overflow at 320px', async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 640 })
  await proxyApi(page)
  await signIn(page, 'admin')
  for (const lang of ['en', 'de'] as const) {
    for (const path of ['/queue', '/admin/settings', '/admin/submissions', '/tickets/new']) {
      await page.goto(withLang(path, lang))
      await expect(page.getByRole('heading', { level: 1 })).toBeVisible()
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), `${lang} ${path}`).toBe(true)
    }
  }
})

test('reduced motion removes the spinner animation', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await signIn(page, 'requester')
  await call(page, '/api/test/ai', { ai_assistance: true })
  await page.goto('/tickets/new')
  await page.getByLabel('Category').selectOption({ label: 'Software' })
  await page.getByLabel('Program').selectOption({ label: 'Word' })
  await page.getByLabel('Subject').fill('Word crashes')
  await page.getByLabel('Description').fill('It closes on startup.')
  await page.getByLabel('Priority', { exact: true }).selectOption({ label: 'High' })
  await page.getByRole('button', { name: 'Submit request' }).click()
  await expect(page.locator('.ai-step .spinner')).toBeVisible()
  await expect(page.locator('.ai-step .spinner')).toHaveCSS('animation-name', 'none')
})
