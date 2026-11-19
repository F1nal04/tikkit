import { execFile } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { promisify } from 'node:util'
import { expect, test, type BrowserContext, type Page } from '@playwright/test'

const password = 'correct horse battery staple!'
const email = 'workflows@example.test'
const screenshot = {
  name: 'workflow.png', mimeType: 'image/png',
  buffer: Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==', 'base64'),
}

const labels = {
  en: {
    username: 'Username', email: 'Email', password: 'Password', signIn: 'Sign in',
    invite: 'Create account', category: 'Category', priority: 'Priority', subject: 'Subject',
    description: 'Description', submit: 'Submit request', receipt: 'Open the ticket',
    confirm: 'Confirm resolved', summary: /^Resolution summary/, propose: 'Propose resolution',
    language: 'Language', signOut: 'Sign out', addScreenshots: 'Add screenshots', handling: 'Handling team',
    general: 'General queue', destination: 'Destination team', dispatch: 'Send to team',
    assigned: 'Assigned to', unassigned: 'Unassigned', claim: 'Claim ticket', start: 'Start work',
    working: 'In progress', message: 'Message', publicReply: 'Send public reply',
    internal: 'Internal note', addNote: 'Add internal note', wait: 'Wait for requester',
    waiting: 'Waiting for requester', resume: 'Resume work', newPriority: 'New priority',
    savePriority: 'Save priority', reject: 'Still not solved', confirmed: 'Confirmed by the requester',
    reopen: 'Reopen ticket', suggestion: 'AI-generated suggestion', solved: 'Solved', help: 'Still need help',
    steps: ['Close the application.', 'Open it again.'], readOnly: /Read-only record/,
    proposalNotice: 'Resolution proposed: confirm or reject it in the portal', replyNotice: 'New public reply',
    allowReopen: 'Allow reopening human-closed tickets', save: 'Save',
  },
  de: {
    username: 'Benutzername', email: 'E-Mail', password: 'Passwort', signIn: 'Anmelden',
    invite: 'Konto anlegen', category: 'Kategorie', priority: 'Priorität', subject: 'Betreff',
    description: 'Beschreibung', submit: 'Anfrage absenden', receipt: 'Ticket öffnen',
    confirm: 'Lösung bestätigen', summary: /^Zusammenfassung der Lösung/, propose: 'Lösung vorschlagen',
    language: 'Sprache', signOut: 'Abmelden', addScreenshots: 'Screenshots hinzufügen',
    handling: 'Bearbeitendes Team', general: 'Allgemeine Warteschlange', destination: 'Zielteam',
    dispatch: 'An Team senden', assigned: 'Zugewiesen an', unassigned: 'Nicht zugewiesen',
    claim: 'Ticket übernehmen', start: 'Bearbeitung beginnen', working: 'In Bearbeitung',
    message: 'Nachricht', publicReply: 'Öffentliche Antwort senden', internal: 'Interne Notiz',
    addNote: 'Interne Notiz hinzufügen', wait: 'Auf Antragsteller warten', waiting: 'Wartet auf Rückmeldung',
    resume: 'Bearbeitung fortsetzen', newPriority: 'Neue Priorität', savePriority: 'Priorität speichern',
    reject: 'Noch nicht gelöst', confirmed: 'Von der anfragenden Person bestätigt',
    reopen: 'Ticket wiedereröffnen', suggestion: 'KI-generierter Vorschlag', solved: 'Gelöst',
    help: 'Ich brauche weiterhin Hilfe', steps: ['Anwendung schließen.', 'Anwendung neu öffnen.'],
    readOnly: /Schreibgeschützter Datensatz/, proposalNotice: 'Lösung vorgeschlagen: im Portal bestätigen oder ablehnen',
    replyNotice: 'Neue öffentliche Antwort', allowReopen: 'Erneutes Öffnen menschlich geschlossener Tickets erlauben',
    save: 'Speichern',
  },
}
type Locale = keyof typeof labels
const setupLocale: Locale = process.env.WORKFLOW_SETUP_LANGUAGE === 'de' ? 'de' : 'en'
const otherLocale: Locale = setupLocale === 'en' ? 'de' : 'en'
const t = labels[setupLocale]
const compose = !!process.env.WORKFLOW_COMPOSE_ENV
const capturedMail = async () => {
  const response = await fetch('http://127.0.0.1:18501/__workflow/mail-count')
  expect(response.status).toBe(200)
  return response.json()
}
const setup = {
  en: { title: 'Set up Tikkit', submit: 'Create administrator' },
  de: { title: 'Tikkit einrichten', submit: 'Administrator anlegen' },
}

function client(page: Page, origin: string) {
  const request = (method: string, path: string, data?: unknown) =>
    page.request.fetch(path, {
      method,
      data,
      headers: { origin, 'x-requested-with': 'tikkit' },
    })
  async function ok(method: string, path: string, data?: unknown, status = 200) {
    const response = await request(method, path, data)
    expect(response.status(), `${method} ${path}`).toBe(status)
    return status === 204 ? undefined : response.json()
  }
  return { request, ok, get: (path: string) => ok('GET', path) }
}

async function open(page: Page, path: string) {
  const response = await page.goto(path)
  await page.waitForLoadState('networkidle')
  return response!
}

const button = (page: Page, name: string) => page.getByRole('button', { name, exact: true })
const status = (page: Page, value: string) => page.locator(`.badge[data-status="${value}"]`)
const fact = (page: Page, name: string) =>
  page.getByRole('term').filter({ hasText: new RegExp(`^${name}$`) }).locator('xpath=following-sibling::dd[1]')
const heading = (page: Page) => page.getByRole('heading', { level: 1 })

async function credentials(page: Page, username: string, locale: Locale, includeEmail = false) {
  const t = labels[locale]
  await page.getByLabel(t.username, { exact: true }).fill(username)
  if (includeEmail) await page.getByLabel(t.email, { exact: true }).fill(email)
  await page.getByLabel(t.password, { exact: true }).fill(password)
}

async function switchLanguage(page: Page, from: Locale, to: Locale) {
  await page.getByLabel(labels[from].language, { exact: true }).selectOption(to)
  await expect(page.locator('html')).toHaveAttribute('lang', to)
}

async function submit(page: Page, subject: string, ai: boolean) {
  await open(page, '/tickets/new')
  await page.getByLabel(t.category, { exact: true }).selectOption({ label: 'Software' })
  await page.getByLabel('Program', { exact: true }).selectOption({ label: 'Writer' })
  await page.getByLabel(t.priority, { exact: true }).selectOption({ label: 'Normal' })
  await page.getByLabel(t.subject, { exact: true }).fill(subject)
  await page.getByLabel(t.description, { exact: true }).fill('Writer closes on startup.')
  if (!ai) {
    // A real preference update and rerender while the intake form holds unsent text.
    await switchLanguage(page, setupLocale, otherLocale)
    await expect(page.getByLabel(labels[otherLocale].subject, { exact: true })).toHaveValue(subject)
    await expect(page.getByLabel(labels[otherLocale].description, { exact: true })).toHaveValue('Writer closes on startup.')
    await switchLanguage(page, otherLocale, setupLocale)
    await expect(page.getByLabel(t.subject, { exact: true })).toHaveValue(subject)
    await page.getByLabel(t.addScreenshots).setInputFiles(screenshot)
    await expect(page.getByRole('img', { name: screenshot.name })).toBeVisible()
  }
  if (ai) {
    await expect(page.getByText(/Skip the AI suggestion|KI-Vorschlag überspringen/i)).toHaveCount(0)
  }
  await button(page, t.submit).click()
  if (!ai) await page.getByRole('link', { name: t.receipt }).click()
  await expect(page).toHaveURL(/\/tickets\/[0-9a-f-]{36}$/)
  return new URL(page.url()).pathname
}

async function propose(page: Page, locale: Locale, summary: string) {
  const t = labels[locale]
  await page.getByLabel(t.summary).fill(summary)
  await button(page, t.propose).click()
  await expect(page.locator('.badge[data-status]')).toHaveText(locale === 'en' ? 'Awaiting confirmation' : 'Wartet auf Bestätigung')
}

// A fresh deployment per run owns bootstrap, feature toggles, and report counts.
test('fresh deployment carries human and AI requests through the real portal', async ({ page: admin, browser, baseURL }) => {
  const origin = new URL(baseURL!).origin
  const api = client(admin, origin)
  const create = (path: string, data: unknown) => api.ok('POST', path, data, 201)
  const contexts: BrowserContext[] = []
  async function actorPage(locale: Locale) {
    const context = await browser.newContext({ baseURL: origin, locale, ignoreHTTPSErrors: compose })
    contexts.push(context)
    return context.newPage()
  }

  try {
    await test.step('bootstrap without AI or SMTP', async () => {
      await open(admin, '/setup?lang=de')
      await expect(heading(admin)).toHaveText('Tikkit einrichten')
      await button(admin, 'Administrator anlegen').click()
      await expect(admin.locator('#form-errors')).toContainText('Bitte korrigieren Sie die markierten Felder.')
      await expect(admin.getByLabel(labels.de.username, { exact: true })).toHaveAttribute('aria-invalid', 'true')
      await open(admin, `/setup?lang=${setupLocale}`)
      await expect(heading(admin)).toHaveText(setup[setupLocale].title)
      await credentials(admin, 'setup-admin', setupLocale, true)
      await button(admin, setup[setupLocale].submit).click()
      await expect(admin).not.toHaveURL(/\/setup/)
      expect(await api.get('/api/auth/me')).toMatchObject({
        role: 'admin', username: 'setup-admin', language: setupLocale,
      })
      expect(await api.get('/api/ai/policy')).toEqual({ ai_assistance: false, requester_skip_allowed: false })
      expect(await api.get('/api/settings/smtp')).toMatchObject({ connection: null })
    })

    async function invite(username: string, role: string | undefined, locale: Locale, ui = false) {
      const issued = await create('/api/invitations', { role, language: locale })
      expect(issued.invitation.role).toBe(role ?? 'user')
      expect(issued.emailed).toBe(false)
      const invited = await actorPage(locale)
      const invitedApi = client(invited, origin)
      const url = new URL(issued.url)
      expect(url.origin).toBe(origin)
      if (ui) {
        await open(invited, `${url.pathname}?lang=${locale}`)
        await expect(invited.locator('html')).toHaveAttribute('lang', locale)
        await credentials(invited, username, locale, true)
        await button(invited, labels[locale].invite).click()
        await expect(invited).not.toHaveURL(/\/invite\//)
        await button(invited, labels[locale].signOut).click()
        await expect(invited).toHaveURL(/\/login/)
        await credentials(invited, username, locale)
        await button(invited, labels[locale].signIn).click()
        await expect(invited).not.toHaveURL(/\/login/)
      } else {
        await invitedApi.ok('POST', '/api/invitations/accept', {
          token: url.pathname.split('/').at(-1), username, email, password, language: locale,
        })
      }
      const identity = await invitedApi.get('/api/auth/me')
      expect(identity).toMatchObject({ role: role ?? 'user', username, email, language: locale })
      return { page: invited, api: invitedApi, identity }
    }

    const requester = await invite('requester', undefined, setupLocale, true)
    const other = await invite('other-requester', 'user', setupLocale, true)
    const lead = await invite('team-lead', 'team_lead', setupLocale)
    const operator = await invite('workflow-operator', 'operator', setupLocale)
    const specialist = await invite('workflow-specialist', 'specialist', setupLocale)
    const secondAdmin = await invite('second-admin', 'admin', setupLocale)
    const team = await create('/api/teams', { name: 'Desktop Support', lead_account_id: lead.identity.id })
    for (const member of [operator, specialist]) {
      await api.ok('PUT', `/api/accounts/${member.identity.id}`, {
        role: member.identity.role, team_id: team.id,
      }, 204)
    }
    const category = await create('/api/categories', { name: 'Software' })
    const normal = await create('/api/priorities', { name: 'Normal' })
    const urgent = await create('/api/priorities', { name: 'Urgent' })
    const program = await create('/api/intake-fields', {
      kind: 'select', label: 'Program', required: true, allow_other: true,
      category_ids: [category.id], choices: [{ label: 'Writer' }],
    })

    const reportQuery = new URLSearchParams({
      created_from: new Date(Date.now() - 86_400_000).toISOString(),
      created_before: new Date(Date.now() + 86_400_000).toISOString(),
    })
    const report = () => api.get(`/api/admin/reports?${reportQuery}`)
    let humanPath = '', shotPath = ''
    await test.step('human work preserves the reference and keeps notes private', async () => {
      const owner = requester.page
      const staff = operator.page
      const subject = 'First-release baseline request'
      humanPath = await submit(owner, subject, false)
      await expect(fact(owner, t.handling)).toHaveText(t.general)
      const id = humanPath.split('/').at(-1)
      const staffPath = `/queue/${id}`
      const original = await requester.api.get(`/api${humanPath}`)
      expect(original.intake_snapshot.screenshot_ids).toHaveLength(1)
      shotPath = `/api/screenshots/${original.intake_snapshot.screenshot_ids[0]}/download`
      const download = await requester.api.request('GET', shotPath)
      expect(download.status()).toBe(200)
      expect(download.headers()['content-type']).toBe('image/png')
      expect((await other.api.request('GET', shotPath)).status()).toBe(404)
      const general = () => operator.api.get('/api/queue?view=general')
      expect((await general()).items.map(({ id }: { id: string }) => id)).toEqual([id])

      await open(staff, staffPath)
      await staff.getByLabel(t.destination).selectOption({ label: team.name })
      await button(staff, t.dispatch).click()
      await expect(fact(staff, 'Team')).toHaveText(team.name)
      await expect(fact(staff, t.assigned)).toHaveText(t.unassigned)
      expect((await general()).items).toEqual([])
      await open(owner, humanPath)
      await expect(fact(owner, t.handling)).toHaveText(team.name)
      await expect(fact(owner, t.assigned)).toHaveText(t.unassigned)

      await button(staff, t.claim).click()
      await expect(fact(staff, t.assigned)).toHaveText(operator.identity.username)
      await button(staff, t.start).click()
      await expect(staff.locator('.badge[data-status]')).toHaveText(t.working)
      await open(owner, humanPath)
      await expect(fact(owner, t.assigned)).toHaveText(operator.identity.username)
      expect((await operator.api.request('GET', shotPath)).status()).toBe(200)
      await expect(heading(owner)).toHaveText(`#${original.reference} · ${subject}`)

      if (!process.env.WORKFLOW_BASE_URL) {
        await api.ok('PUT', '/api/settings/smtp', {
          host: '127.0.0.1', port: 18503, security: 'starttls', sender_name: 'Workflow Desk',
          sender_address: 'help@example.test', username: 'mailer',
          password: ['smtp', 'secret', 'value'].join('-'), enabled: true,
        })
      }
      const deliveries = () => api.get('/api/admin/notifications')
      const inbox = async () => (await requester.api.get('/api/notifications')).items
        .filter((entry: { ticket_id: string }) => entry.ticket_id === id)
      await expect.poll(async () => (await deliveries()).pending).toBe(0)
      const sentBefore = (await deliveries()).sent
      if (!compose) expect(await capturedMail()).toBe(0)
      const publicReply = 'The support team is investigating.'
      const privateNote = 'PRIVATE-WORKFLOW-NOTE'
      await staff.getByLabel(t.message, { exact: true }).fill(publicReply)
      await button(staff, t.publicReply).click()
      await expect(staff.getByText(publicReply, { exact: true })).toBeVisible()
      // The public reply notifies just the requester: one inbox item, plus one
      // email when the local STARTTLS capture is configured.
      const sentAfterReply = sentBefore + (process.env.WORKFLOW_BASE_URL ? 1 : 2)
      await expect.poll(deliveries).toMatchObject({ sent: sentAfterReply, pending: 0, failed: 0 })
      if (!compose) expect(await capturedMail()).toBe(1)
      const afterReply = await inbox()
      expect(afterReply.filter((entry: { title: string }) => entry.title === t.replyNotice)).toHaveLength(1)
      await staff.getByLabel(t.internal, { exact: true }).check()
      await staff.getByLabel(t.message, { exact: true }).fill(privateNote)
      await button(staff, t.addNote).click()
      await expect(staff.getByText(privateNote, { exact: true })).toBeVisible()

      const html = await open(owner, humanPath)
      expect(await html.text()).not.toContain(privateNote)
      await expect(owner.getByText(publicReply, { exact: true })).toBeVisible()
      await expect(owner.getByText(privateNote, { exact: true })).toHaveCount(0)
      const messages = await requester.api.get(`/api${humanPath}/messages`)
      expect(messages.items.map(({ body }: { body: string }) => body)).toEqual([publicReply])
      expect(await deliveries()).toMatchObject({ sent: sentAfterReply, pending: 0, failed: 0 })
      expect(await inbox()).toEqual(afterReply)
      if (!compose) expect(await capturedMail()).toBe(1)

      await button(staff, t.wait).click()
      await expect(staff.locator('.badge[data-status]')).toHaveText(t.waiting)
      await button(staff, t.resume).click()
      await expect(status(staff, 'in_progress')).toBeVisible()
      for (const [from, to] of [[normal, urgent], [urgent, normal]]) {
        await staff.getByLabel(t.newPriority).selectOption(to.id)
        await button(staff, t.savePriority).click()
        await expect(staff.getByText(`${from.name} → ${to.name}`)).toBeVisible()
      }
      await propose(staff, setupLocale, 'Repaired the initial profile.')
      await open(owner, humanPath)
      await button(owner, t.reject).click()
      await expect(status(owner, 'in_progress')).toBeVisible()
      await expect(fact(owner, t.handling)).toHaveText(team.name)
      await open(staff, staffPath)
      await propose(staff, setupLocale, 'Repaired the backup profile as well.')
      await open(owner, humanPath)
      await button(owner, t.confirm).click()
      await expect(status(owner, 'closed')).toBeVisible()
      await expect(owner.getByText(t.confirmed, { exact: true })).toBeVisible()
      await expect(button(owner, t.reopen)).toHaveCount(0)
      expect(await report()).toMatchObject({
        submissions: 1,
        operator: { initial_requester_confirmed: 1, initial_confirmation_timeout: 0, rejected_proposals: 1 },
      })
    })

    let solvedPath = ''
    await test.step('real provider and policy produce one final AI solution', async () => {
      const conf = await api.get('/api/settings/ai')
      const openai = conf.providers.find(({ provider }: { provider: string }) => provider === 'openai')
      await api.ok('PUT', '/api/settings/ai/credentials/openai', { api_key: ['workflow', 'fake', 'key'].join('-') })
      await api.ok('PUT', '/api/settings/ai', { enabled: true, provider: 'openai', model: openai.models[0].id })
      const skip = await requester.api.request('POST', '/api/tickets', {
        idempotency_key: crypto.randomUUID(), requester_skip: true,
        intake: {
          category_id: category.id, priority_id: normal.id, subject: 'Forged skip',
          description: 'A requester cannot bypass mandatory AI.',
          answers: { [program.id]: { choice_id: program.choices[0].id } },
          screenshot_ids: [],
        },
      })
      expect(skip.status()).toBe(403)

      const owner = requester.page
      const subject = 'AI-only support request'
      const path = await submit(owner, subject, true)
      solvedPath = path
      await expect(owner.getByRole('heading', { name: t.suggestion })).toBeVisible()
      const suggestion = owner.getByTestId('ai-suggestion')
      await expect(suggestion).toHaveAttribute('lang', setupLocale)
      await expect(suggestion.getByRole('listitem')).toHaveText(t.steps)
      await switchLanguage(owner, setupLocale, otherLocale)
      expect((await requester.api.get('/api/auth/me')).language).toBe(otherLocale)
      await expect(heading(owner)).toContainText(subject)
      await expect(suggestion).toHaveAttribute('lang', setupLocale)
      await expect(owner.locator('section.ai-step').getByRole('button')).toHaveText([labels[otherLocale].solved, labels[otherLocale].help])
      await switchLanguage(owner, otherLocale, setupLocale)
      await button(owner, t.solved).dblclick()
      await expect(status(owner, 'ai_solved')).toBeVisible()
      await owner.reload()
      await expect(status(owner, 'ai_solved')).toBeVisible()
      await expect(owner.locator('html')).toHaveAttribute('lang', setupLocale)
      await expect(owner.getByRole('main').getByRole('textbox')).toHaveCount(0)

      const id = path.split('/').at(-1)
      const queuePath = '/api/queue?view=general'
      expect((await operator.api.get(queuePath)).items).toEqual([])
      await open(operator.page, '/queue?view=general')
      await expect(operator.page.getByRole('link', { name: subject })).toHaveCount(0)
      expect((await operator.api.request('GET', `/api${path}`)).status()).toBe(404)
      expect((await other.api.request('GET', `/api${path}`)).status()).toBe(404)
      await open(admin, `/admin/submissions/${id}`)
      await expect(heading(admin)).toContainText(subject)
      await expect(admin.getByText(t.readOnly)).toBeVisible()
      expect(await report()).toMatchObject({ submissions: 2, ai_attempts: 1, ai_rate_numerator: 1 })
      expect(await api.get('/api/settings/ai/usage')).toMatchObject({ totals: { requests: 1, succeeded: 1 } })
    })

    await test.step('AI handoff enters the same human workflow and survives a policy change', async () => {
      const owner = other.page
      const subject = 'Request needing human help'
      const path = await submit(owner, subject, true)
      await expect(owner.getByRole('heading', { name: t.suggestion })).toBeVisible()
      const suggestion = owner.getByTestId('ai-suggestion')
      await expect(suggestion).toHaveAttribute('lang', setupLocale)
      await expect(suggestion.getByRole('listitem')).toHaveText(t.steps)
      await button(owner, t.help).click()
      await expect(status(owner, 'queued')).toBeVisible()

      const id = path.split('/').at(-1)
      expect((await operator.api.get('/api/queue?view=general')).items.map(({ id }: { id: string }) => id)).toEqual([id])
      const staff = specialist.page
      await open(staff, `/queue/${id}`)
      await staff.getByLabel(t.destination).selectOption({ label: team.name })
      await button(staff, t.dispatch).click()
      await expect(fact(staff, 'Team')).toHaveText(team.name)
      await button(staff, t.claim).click()
      await expect(fact(staff, t.assigned)).toHaveText(specialist.identity.username)
      await button(staff, t.start).click()
      await expect(staff.locator('.badge[data-status]')).toHaveText(t.working)
      await propose(staff, setupLocale, 'Das Profil wurde wiederhergestellt.')
      await open(owner, path)
      await button(owner, t.confirm).click()
      await expect(status(owner, 'closed')).toBeVisible()
      await expect(owner.getByText(t.confirmed, { exact: true })).toBeVisible()
      await expect.poll(async () => (await other.api.get('/api/notifications')).items).toContainEqual(
        expect.objectContaining({
          ticket_id: id, href: path, title: t.proposalNotice,
        }),
      )

      const settings = secondAdmin.page
      await open(settings, '/admin/settings')
      const toggle = settings.getByLabel(t.allowReopen)
      await expect(toggle).not.toBeChecked()
      await toggle.check()
      const saved = settings.waitForResponse((r) => r.url().endsWith('/api/admin/settings') && r.request().method() === 'PUT')
      await button(settings, t.save).click()
      expect((await saved).ok()).toBe(true)
      await open(owner, path)
      await button(owner, t.reopen).click()
      await expect(status(owner, 'in_progress')).toBeVisible()
      expect(await report()).toMatchObject({
        submissions: 3, ai_attempts: 2, ai_rate_numerator: 1,
        operator: { initial_requester_confirmed: 2, reopens: 1 },
      })
      const refused = await requester.api.request('POST', `/api${solvedPath}/actions`, { action: 'reopen' })
      expect(refused.status()).toBe(409)
      expect(await refused.json()).toMatchObject({ error: { code: 'conflict' } })
      await open(requester.page, solvedPath)
      await expect(status(requester.page, 'ai_solved')).toBeVisible()
      await expect(button(requester.page, t.reopen)).toHaveCount(0)
      await open(requester.page, humanPath)
      await expect(status(requester.page, 'closed')).toBeVisible()
    })

    if (compose) {
      await test.step('official HTTPS provider rejection hands off exactly once', async () => {
        const failed = await submit(requester.page, 'Compose provider rejection fixture', true)
        await expect(status(requester.page, 'queued')).toBeVisible()
        const detail = await requester.api.get(`/api${failed}`)
        expect(detail.transitions.filter((event: { event_type: string }) => event.event_type === 'ai_failed')).toHaveLength(1)
        expect(detail.ai).toMatchObject({ state: 'handed_off', reason: 'ai_failed', suggestion: null })
        expect((await operator.api.get('/api/queue?view=general')).items).toMatchObject([{ id: detail.id }])
        expect(await api.get('/api/settings/ai/usage')).toMatchObject({
          totals: { requests: 3, succeeded: 2, failed: 1 },
          outcomes: expect.arrayContaining([expect.objectContaining({ outcome: 'invalid_credentials', count: 1 })]),
        })
        expect(await report()).toMatchObject({ submissions: 4, ai_attempts: 3, ai_rate_numerator: 1 })
      })

      await test.step('recreated application containers preserve portal data and exclusion', async () => {
        const beforeTicket = await requester.api.get(`/api${humanPath}`)
        expect(beforeTicket).toMatchObject({ status: 'closed', closure_reason: 'requester_confirmed' })
        const beforeMessages = await requester.api.get(`/api${humanPath}/messages`)
        const beforeReport = await report()
        const beforeSolved = await requester.api.get(`/api${solvedPath}`)
        const download = async (session: ReturnType<typeof client>) => {
          const response = await session.request('GET', shotPath)
          expect(response.status()).toBe(200)
          return response.body()
        }
        const beforeImage = await download(requester.api)

        await promisify(execFile)('bash', [
          fileURLToPath(new URL('../../../ops/compose/workflows.sh', import.meta.url)), 'recreate',
        ], { timeout: 150_000 })

        const session = async (username: string) => {
          const page = await actorPage(setupLocale)
          await open(page, '/login')
          await credentials(page, username, setupLocale)
          await button(page, t.signIn).click()
          await expect(page).not.toHaveURL(/\/login/)
          return { page, api: client(page, origin) }
        }
        const owner = await session(requester.identity.username)
        expect(await owner.api.get(`/api${humanPath}`)).toEqual(beforeTicket)
        expect(await owner.api.get(`/api${humanPath}/messages`)).toEqual(beforeMessages)
        expect(await download(owner.api)).toEqual(beforeImage)
        expect(await owner.api.get(`/api${solvedPath}`)).toEqual(beforeSolved)
        await open(owner.page, humanPath)
        await expect(heading(owner.page)).toContainText(`#${beforeTicket.reference}`)
        await expect(owner.page.getByText(t.confirmed, { exact: true })).toBeVisible()
        const afterAdmin = await session('setup-admin')
        expect(await afterAdmin.api.get(`/api/admin/reports?${reportQuery}`)).toEqual(beforeReport)
        const afterOperator = await session(operator.identity.username)
        expect((await afterOperator.api.request('GET', `/api${solvedPath}`)).status()).toBe(404)
        const hits = await afterOperator.api.get(`/api/queue?view=general&q=${encodeURIComponent(`#${beforeSolved.reference}`)}`)
        expect(hits.items).toEqual([])
      })
    }
  } finally {
    await Promise.all(contexts.map((context) => context.close()))
  }
})
