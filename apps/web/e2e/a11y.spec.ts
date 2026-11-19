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
  const overflow = await page.evaluate(() => {
    const width = window.innerWidth
    if (document.documentElement.scrollWidth <= width) return ''
    const wide = [...document.querySelectorAll('body *')]
      .filter((el) => (el as HTMLElement).scrollWidth > width + 1)
      .slice(0, 6)
      .map((el) => `${el.tagName}.${String((el as HTMLElement).className).slice(0, 40)} ${(el as HTMLElement).scrollWidth}`)
    return wide.join(' | ')
  })
  if (overflow) problems.push(`${label}: horizontal overflow ${overflow}`)
  const results = await new AxeBuilder({ page }).withTags(TAGS).analyze()
  for (const violation of results.violations) {
    if (violation.id in EXCLUSIONS) continue
    problems.push(`${label}: ${formatViolation(violation)}`)
  }
  // Incomplete color-contrast is a real gap, not a pass. Disabled catalog reorder buttons
  // (opacity 0.4 at either end of a list) are the one case axe cannot score; that reason is
  // in docs/accessibility.md. Any other incomplete node fails the scan.
  const contrast = results.incomplete.filter((item) => item.id === 'color-contrast')
  const unexplained = contrast.flatMap((item) => item.nodes.filter((node) => {
    const target = (Array.isArray(node.target) ? node.target : [node.target]).join(' ')
    return !/Move (up|down)|Nach (oben|unten)/.test(target)
  }))
  if (unexplained.length) {
    problems.push(`${label}: color-contrast incomplete\n${contrast.map((item) => formatViolation(item)).join('\n')}`)
  }
  return problems
}

function routeOf(path: string) {
  return path
    .replace(/^\/invite\/.*/, '/invite/[token]')
    .replace(/^\/reset-password\/.*/, '/reset-password/[token]')
    .replace(/^\/tickets\/(?!new$).*/, '/tickets/[id]')
    .replace(/^\/queue\/.*/, '/queue/[id]')
    .replace(/^\/admin\/submissions\/.*/, '/admin/submissions/[id]')
}

const HEADS: Record<string, { en: string | RegExp; de: string | RegExp }> = {
  '/login': { en: 'Sign in', de: 'Anmelden' },
  '/forgot-password': { en: 'Reset your password', de: 'Passwort zurücksetzen' },
  '/reset-password/[token]': { en: 'Choose a new password', de: 'Neues Passwort festlegen' },
  '/invite/[token]': { en: 'Create your Tikkit account', de: 'Tikkit-Konto anlegen' },
  '/setup': { en: 'Set up Tikkit', de: 'Tikkit einrichten' },
  '/': { en: 'My tickets', de: 'Meine Tickets' },
  '/tickets/new': { en: 'New ticket', de: 'Neues Ticket' },
  '/profile': { en: 'Your profile', de: 'Ihr Profil' },
  '/notifications': { en: 'Notifications', de: 'Benachrichtigungen' },
  '/queue': { en: 'General queue', de: 'Allgemeine Warteschlange' },
  [`/queue/${GENERAL}`]: { en: '#1 · General ticket 1', de: '#1 · General ticket 1' },
  [`/queue/${DESK}`]: { en: '#101 · Desk unclaimed', de: '#101 · Desk unclaimed' },
  [`/queue/${CLAIMED}`]: { en: '#102 · Desk claimed', de: '#102 · Desk claimed' },
  '/team': { en: 'My team', de: 'Mein Team' },
  '/admin': { en: 'Administration', de: 'Administration' },
  '/admin/accounts': { en: 'Accounts', de: 'Konten' },
  '/admin/teams': { en: 'Teams', de: 'Teams' },
  '/admin/settings': { en: 'Organization settings', de: 'Organisationseinstellungen' },
  '/admin/email': { en: 'Outbound email', de: 'Ausgehende E-Mail' },
  '/admin/ai': { en: 'AI assistance', de: 'KI-Unterstützung' },
  '/admin/ai/records': { en: 'AI outcomes', de: 'KI-Ergebnisse' },
  '/admin/catalog': { en: 'Ticket catalog', de: 'Ticketkatalog' },
  '/admin/submissions': { en: 'Submissions', de: 'Einreichungen' },
  [`/admin/submissions/${SUBMISSION}`]: { en: '#301 · Solved by the AI suggestion', de: '#301 · Solved by the AI suggestion' },
  '/admin/audit': { en: 'Audit log', de: 'Audit-Protokoll' },
  '/admin/reports': { en: 'Reports', de: 'Berichte' },
  '/admin/retention': { en: 'Data retention', de: 'Datenaufbewahrung' },
}

/** Count a route only after the visited URL and its own heading are on screen. */
async function open(page: Page, lang: Lang, path: string, seen: Set<string>, heading?: string | RegExp) {
  const route = routeOf(path)
  await page.goto(withLang(path, lang))
  await expect(page).toHaveURL(new RegExp(`${path.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(?:\\?|$)`))
  const expected = heading ?? HEADS[path]?.[lang] ?? HEADS[route]?.[lang]
  if (!expected) return [`${lang} ${path}: no expected heading`]
  await expect(page.getByRole('heading', { level: 1 })).toHaveText(expected)
  seen.add(route)
  return inspect(page, lang, `${lang} ${route} (${path})`)
}

async function signInAs(page: Page, username: string, lang: Lang) {
  // A ?lang= visit stores a cookie, which would relabel the sign-in form.
  await page.context().clearCookies()
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
  const de = lang === 'de'
  // The form, with the conditional program field and a screenshot, before it is sent.
  await page.goto(withLang('/tickets/new', lang))
  await page.waitForLoadState('networkidle')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText(de ? 'Neues Ticket' : 'New ticket')
  seen.add('/tickets/new')
  await page.getByLabel(de ? 'Kategorie' : 'Category').selectOption({ label: 'Software' })
  await page.getByLabel('Program').selectOption({ label: 'Word' })
  await page.getByLabel(de ? 'Screenshots hinzufügen' : 'Add screenshots').setInputFiles({
    name: 'error.png', mimeType: 'image/png', buffer: PNG,
  })
  await expect(page.getByRole('img', { name: 'error.png' })).toBeVisible()
  problems.push(...await inspect(page, lang, `${lang} /tickets/new`))

  const processing = await fillAndSubmit(page, lang)
  await expect(page).toHaveURL(new RegExp(`/tickets/${processing}(?:\\?|$)`))
  await expect(page.getByRole('heading', { level: 1 })).toContainText('Word crashes')
  seen.add('/tickets/[id]')
  problems.push(...await inspect(page, lang, `${lang} /tickets/[id] processing`))
  await expect(page.locator('.ai-step')).toHaveAttribute('aria-busy', 'true')

  await call(page, '/api/test/ai-result', { id: processing, outcome: 'suggestion' })
  await page.goto(withLang(`/tickets/${processing}`, lang))
  await expect(page).toHaveURL(new RegExp(`/tickets/${processing}(?:\\?|$)`))
  await expect(page.getByRole('heading', { level: 1 })).toContainText('Word crashes')
  problems.push(...await inspect(page, lang, `${lang} /tickets/[id] suggestion`))
  // signInAs set the account language before this ticket was submitted, so the stored suggestion uses that language.
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
  await expect(page.getByRole('heading', { level: 1 })).toHaveText(lang === 'de' ? 'Meine Tickets' : 'My tickets')
  seen.add('/')
  problems.push(...await inspect(page, lang, `${lang} /`))
  await page.goto(withLang(`/tickets/${history.pending}`, lang))
  await expect(page).toHaveURL(new RegExp(`/tickets/${history.pending}(?:\\?|$)`))
  await expect(page.getByRole('heading', { level: 1 })).toContainText('Resolution pending')
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

  problems.push(...await scanStates(page, lang))

  const missing = COVERED.filter((route) => !seen.has(route))
  if (missing.length) problems.push(`not scanned: ${missing.join(', ')}`)
  return problems
}

/** States the route scan does not open on its own: dialogs, invalid forms, banners, bulk actions. */
async function scanStates(page: Page, lang: Lang) {
  const problems: string[] = []
  const de = lang === 'de'
  const state = async (label: string) => {
    problems.push(...await inspect(page, lang, `${lang} ${label}`))
  }

  await signInAs(page, 'requester', lang)
  await page.goto(withLang('/tickets/new', lang))
  await page.waitForLoadState('networkidle')
  await page.getByRole('button', { name: de ? 'Anfrage absenden' : 'Submit request' }).click()
  await expect(page.locator('#form-errors')).toBeVisible()
  await expect(page.getByLabel(de ? 'Kategorie' : 'Category')).toHaveAttribute('aria-invalid', 'true')
  await state('ticket form validation')

  await page.getByLabel(de ? 'Betreff' : 'Subject').fill('Draft to discard')
  await expect(page.getByText(/Draft saved at|Entwurf um/)).toBeVisible()
  await page.reload()
  await page.getByRole('button', { name: de ? /Entwurf verwerfen/ : /Discard draft/ }).click()
  await expect(page.getByRole('dialog')).toBeVisible()
  await state('discard dialog')
  await page.keyboard.press('Escape')

  await signOut(page)
  await page.goto(withLang('/login', lang))
  await page.getByLabel(de ? 'Benutzername' : 'Username').fill('requester')
  await page.getByLabel(de ? 'Passwort' : 'Password').fill('wrong password')
  await page.getByRole('button', { name: de ? 'Anmelden' : 'Sign in' }).click()
  await expect(page.locator('#form-errors')).toBeVisible()
  await state('login validation')

  await signInAs(page, 'operator', lang)
  await page.goto(withLang('/queue', lang))
  await page.getByRole('checkbox', { name: de ? 'Alle Tickets dieser Seite auswählen' : 'Select all tickets on this page' }).check()
  await expect(page.getByRole('form', { name: de ? 'Ausgewählte Tickets ändern' : 'Change selected tickets' })).toBeVisible()
  await state('queue bulk actions')
  await page.goto(withLang(`/queue/${DESK}`, lang))
  await expect(page.getByLabel(de ? 'Zielteam' : 'Destination team')).toBeVisible()
  await state('dispatch controls')

  await signInAs(page, 'lead', lang)
  await page.goto(withLang(`/queue/${DESK}`, lang))
  await expect(page.getByLabel(de ? 'Zuweisen an' : 'Assign to')).toBeVisible()
  await state('assign controls')

  await signInAs(page, 'admin', lang)
  await page.goto(withLang('/admin/settings', lang))
  await page.route('**/api/admin/settings', (route) => {
    if (route.request().method() !== 'PUT') return route.fallback()
    return route.fulfill({ status: 422, contentType: 'application/json', body: JSON.stringify({ error: { code: 'validation_failed', details: { fields: { timezone: ['invalid'] } } }, request_id: 'axe' }) })
  })
  await page.getByRole('button', { name: de ? 'Speichern' : 'Save', exact: true }).click()
  await expect(page.locator('#form-errors')).toBeVisible()
  await expect(page.getByLabel(de ? 'Zeitzone' : 'Timezone')).toHaveAttribute('aria-invalid', 'true')
  await state('settings validation')
  await page.unroute('**/api/admin/settings')

  await page.goto(withLang('/admin/retention', lang))
  await page.locator('input[name="draft_days"]').fill('30')
  await page.getByRole('checkbox').first().check()
  await page.getByRole('button', { name: de ? 'Löschvorschau' : 'Preview deletion' }).click()
  await expect(page.getByTestId('preview')).toBeVisible()
  await state('retention preview')
  await page.locator('input[name="save_confirm"]').fill('DELETE')
  await state('retention confirmation')

  await page.evaluate(async () => {
    await fetch('/api/test/restore-mode', {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' },
      body: JSON.stringify({ on: true }),
    })
  })
  await page.goto(withLang('/admin', lang))
  await expect(page.getByText(de ? 'Der Wiederherstellungsmodus ist aktiv' : 'Restore mode is on')).toBeVisible()
  await state('restore mode')
  await page.evaluate(async () => {
    await fetch('/api/test/restore-mode', {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' },
      body: JSON.stringify({ on: false }),
    })
  })

  await page.goto(withLang('/admin/accounts', lang))
  await page.getByText(de ? 'Konto bearbeiten: admin' : 'Edit account: admin', { exact: true }).click()
  await expect(page.getByRole('article', { name: 'admin', exact: true }).getByLabel(de ? 'Rolle' : 'Role')).toBeVisible()
  await state('accounts after edit')

  await page.goto(withLang('/admin/settings', lang))
  await page.evaluate(() => fetch('/api/test/expire', { method: 'POST' }))
  await page.getByRole('button', { name: de ? 'Speichern' : 'Save', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: de ? 'abgelaufen' : 'expired' })).toBeVisible()
  await state('session expired')
  return problems
}

for (const viewport of VIEWPORTS) {
  for (const lang of ['en', 'de'] as const) {
    test(`${lang} ${viewport.name} has no WCAG 2.2 A/AA violations`, async ({ page }) => {
      test.setTimeout(360_000)
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

test('each queue change is announced, including the second and third', async ({ page }) => {
  await signIn(page, 'operator')
  await page.goto('/queue')
  const live = page.getByTestId('queue-announcement')
  await expect(live).toHaveText('')
  await expect(live).toHaveAttribute('data-updates', '0')
  for (const reference of [1, 2, 3]) {
    const status = await page.evaluate(async (reference) => {
      const id = `30000000-0000-4000-8000-${String(reference).padStart(12, '0')}`
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
    }, reference)
    expect(status).toBe(200)
    await page.evaluate(() => document.dispatchEvent(new Event('visibilitychange')))
    await expect(live).toHaveAttribute('data-updates', String(reference))
    await expect(live).toHaveText('The queue was updated.')
  }
  await page.evaluate(() => document.dispatchEvent(new Event('visibilitychange')))
  await expect(live).toHaveAttribute('data-updates', '3')
})

test('text at 200% stays on the page in both languages', async ({ page }) => {
  test.setTimeout(90_000)
  await proxyApi(page)
  const fits = async (label: string) => {
    await page.evaluate(() => { document.documentElement.style.fontSize = '32px' })
    await expect(page.getByRole('heading', { level: 1 })).toBeVisible()
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), label).toBe(true)
  }
  for (const lang of ['en', 'de'] as const) {
    await page.setViewportSize({ width: 1280, height: 800 })
    await page.goto(withLang('/login', lang))
    await fits(`${lang} login`)
  }
  await page.context().clearCookies()
  await signIn(page, 'requester')
  const history = (await call(page, '/api/test/history', {})) as { pending: string }
  for (const lang of ['en', 'de'] as const) {
    await page.setViewportSize({ width: 1280, height: 800 })
    await page.goto(withLang('/tickets/new', lang))
    await fits(`${lang} form`)
    await page.goto(withLang(`/tickets/${history.pending}`, lang))
    await fits(`${lang} ticket`)
  }
  await page.context().clearCookies()
  await signIn(page, 'operator')
  for (const lang of ['en', 'de'] as const) {
    await page.goto(withLang(`/queue/${DESK}`, lang))
    await fits(`${lang} queue detail`)
  }
})

test('a phone-width page does not overflow at 320px and actions stay reachable', async ({ page }) => {
  test.setTimeout(90_000)
  await page.setViewportSize({ width: 320, height: 640 })
  await proxyApi(page)
  const withinWidth = async (label: string) => {
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), label).toBe(true)
    const clipped = await page.evaluate(() => [...document.querySelectorAll('.table-wrap')].filter((el) => {
      const region = el as HTMLElement
      return region.scrollWidth > region.clientWidth + 1 && region.tabIndex < 0
    }).length)
    expect(clipped, `${label} table`).toBe(0)
  }
  await signIn(page, 'requester')
  const history = (await call(page, '/api/test/history', {})) as { pending: string }
  for (const lang of ['en', 'de'] as const) {
    await page.goto(withLang(`/tickets/${history.pending}`, lang))
    await expect(page.getByRole('heading', { level: 1 })).toBeVisible()
    await withinWidth(`${lang} ticket`)
    const confirm = page.getByRole('button', { name: lang === 'de' ? 'Lösung bestätigen' : 'Confirm resolved' })
    const box = await confirm.boundingBox()
    expect(box, `${lang} confirm`).toBeTruthy()
    expect(box!.x).toBeGreaterThanOrEqual(0)
    expect(box!.x + box!.width).toBeLessThanOrEqual(321)
  }
  await page.context().clearCookies()
  await signIn(page, 'operator')
  for (const lang of ['en', 'de'] as const) {
    await page.goto(withLang(`/queue/${DESK}`, lang))
    await expect(page.getByRole('heading', { level: 1 })).toBeVisible()
    await withinWidth(`${lang} queue detail`)
    const claim = page.getByRole('button', { name: lang === 'de' ? 'Ticket übernehmen' : 'Claim ticket' })
    const box = await claim.boundingBox()
    expect(box, `${lang} claim`).toBeTruthy()
    expect(box!.x).toBeGreaterThanOrEqual(0)
    expect(box!.x + box!.width).toBeLessThanOrEqual(321)
  }
  await page.context().clearCookies()
  await signIn(page, 'admin')
  for (const lang of ['en', 'de'] as const) {
    for (const path of ['/queue', '/admin/settings', '/admin/submissions', '/tickets/new']) {
      await page.goto(withLang(path, lang))
      await expect(page.getByRole('heading', { level: 1 })).toBeVisible()
      await withinWidth(`${lang} ${path}`)
    }
  }
})

test('reduced motion stops every animation', async ({ page }) => {
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
  const moving = await page.evaluate(() => [...document.querySelectorAll('*')].filter((el) => {
    const style = getComputedStyle(el)
    return style.animationName !== 'none' && parseFloat(style.animationDuration) > 0.05
  }).map((el) => (el as HTMLElement).className || el.tagName))
  expect(moving).toEqual([])
})
