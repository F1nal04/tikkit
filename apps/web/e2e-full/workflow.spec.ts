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
  },
  de: {
    username: 'Benutzername', email: 'E-Mail', password: 'Passwort', signIn: 'Anmelden',
    invite: 'Konto anlegen', category: 'Kategorie', priority: 'Priorität', subject: 'Betreff',
    description: 'Beschreibung', submit: 'Anfrage absenden', receipt: 'Ticket öffnen',
    confirm: 'Lösung bestätigen', summary: /^Zusammenfassung der Lösung/, propose: 'Lösung vorschlagen',
  },
}
type Locale = keyof typeof labels
const setupLocale: Locale = process.env.WORKFLOW_SETUP_LANGUAGE === 'de' ? 'de' : 'en'
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

async function submit(page: Page, subject: string, locale: Locale, ai: boolean) {
  const t = labels[locale]
  await open(page, '/tickets/new')
  await page.getByLabel(t.category, { exact: true }).selectOption({ label: 'Software' })
  await page.getByLabel('Program', { exact: true }).selectOption({ label: 'Writer' })
  await page.getByLabel(t.priority, { exact: true }).selectOption({ label: 'Normal' })
  await page.getByLabel(t.subject, { exact: true }).fill(subject)
  await page.getByLabel(t.description, { exact: true }).fill('Writer closes on startup.')
  if (!ai) {
    await page.getByLabel('Add screenshots').setInputFiles(screenshot)
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
  const origin = baseURL!
  const api = client(admin, origin)
  const create = (path: string, data: unknown) => api.ok('POST', path, data, 201)
  const contexts: BrowserContext[] = []
  async function actorPage(locale: Locale) {
    const context = await browser.newContext({ baseURL: origin, locale })
    contexts.push(context)
    return context.newPage()
  }

  try {
    await test.step('bootstrap without AI or SMTP', async () => {
      await open(admin, '/setup?lang=de')
      await expect(heading(admin)).toHaveText('Tikkit einrichten')
      await button(admin, 'Administrator anlegen').click()
      await expect(admin.getByRole('alert')).toContainText('Bitte korrigieren Sie die markierten Felder.')
      await expect(admin.getByLabel(labels.de.username, { exact: true })).toHaveAttribute('aria-invalid', 'true')
      await open(admin, `/setup?lang=${setupLocale}`)
      await expect(heading(admin)).toHaveText(setup[setupLocale].title)
      await credentials(admin, 'setup-admin', setupLocale, true)
      await button(admin, setup[setupLocale].submit).click()
      await expect(admin).not.toHaveURL(/\/setup/)
      expect(await api.get('/api/auth/me')).toMatchObject({
        role: 'admin', username: 'setup-admin', language: setupLocale,
      })
      if (setupLocale === 'de') {
        await admin.getByLabel('Sprache', { exact: true }).selectOption('en')
        await expect(admin.locator('html')).toHaveAttribute('lang', 'en')
      }
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
        await button(invited, locale === 'en' ? 'Sign out' : 'Abmelden').click()
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

    const requester = await invite('requester-en', undefined, 'en', true)
    const german = await invite('requester-de', 'user', 'de', true)
    const lead = await invite('team-lead', 'team_lead', 'en')
    const operator = await invite('workflow-operator', 'operator', 'en')
    const specialist = await invite('workflow-specialist', 'specialist', 'de')
    const secondAdmin = await invite('second-admin', 'admin', 'de')
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
    let humanPath = ''
    await test.step('human work preserves the reference and keeps notes private', async () => {
      const owner = requester.page
      const staff = operator.page
      const subject = 'First-release baseline request'
      humanPath = await submit(owner, subject, 'en', false)
      await expect(fact(owner, 'Handling team')).toHaveText('General queue')
      const id = humanPath.split('/').at(-1)
      const staffPath = `/queue/${id}`
      const original = await requester.api.get(`/api${humanPath}`)
      expect(original.intake_snapshot.screenshot_ids).toHaveLength(1)
      const shotPath = `/api/screenshots/${original.intake_snapshot.screenshot_ids[0]}/download`
      const download = await requester.api.request('GET', shotPath)
      expect(download.status()).toBe(200)
      expect(download.headers()['content-type']).toBe('image/png')
      expect((await german.api.request('GET', shotPath)).status()).toBe(404)
      const general = () => operator.api.get('/api/queue?view=general')
      expect((await general()).items.map(({ id }: { id: string }) => id)).toEqual([id])

      await open(staff, staffPath)
      await staff.getByLabel('Destination team').selectOption({ label: team.name })
      await button(staff, 'Send to team').click()
      await expect(fact(staff, 'Team')).toHaveText(team.name)
      await expect(fact(staff, 'Assigned to')).toHaveText('Unassigned')
      expect((await general()).items).toEqual([])
      await open(owner, humanPath)
      await expect(fact(owner, 'Handling team')).toHaveText(team.name)
      await expect(fact(owner, 'Assigned to')).toHaveText('Unassigned')

      await button(staff, 'Claim ticket').click()
      await expect(fact(staff, 'Assigned to')).toHaveText(operator.identity.username)
      await button(staff, 'Start work').click()
      await expect(staff.locator('.badge[data-status]')).toHaveText('In progress')
      await open(owner, humanPath)
      await expect(fact(owner, 'Assigned to')).toHaveText(operator.identity.username)
      expect((await operator.api.request('GET', shotPath)).status()).toBe(200)
      await expect(heading(owner)).toHaveText(`#${original.reference} · ${subject}`)

      if (!process.env.WORKFLOW_BASE_URL) {
        await api.ok('PUT', '/api/settings/smtp', {
          host: '127.0.0.1', port: 18503, security: 'starttls', sender_name: 'Workflow Desk',
          sender_address: 'help@example.test', username: 'mailer',
          password: ['smtp', 'secret', 'value'].join('-'), enabled: true,
        })
      }
      const sentBefore = (await api.get('/api/admin/notifications')).sent
      const publicReply = 'The support team is investigating.'
      const privateNote = 'PRIVATE-WORKFLOW-NOTE'
      await staff.getByLabel('Message', { exact: true }).fill(publicReply)
      await button(staff, 'Send public reply').click()
      await expect(staff.getByText(publicReply, { exact: true })).toBeVisible()
      await staff.getByLabel('Internal note', { exact: true }).check()
      await staff.getByLabel('Message', { exact: true }).fill(privateNote)
      await button(staff, 'Add internal note').click()
      await expect(staff.getByText(privateNote, { exact: true })).toBeVisible()

      const html = await open(owner, humanPath)
      expect(await html.text()).not.toContain(privateNote)
      await expect(owner.getByText(publicReply, { exact: true })).toBeVisible()
      await expect(owner.getByText(privateNote, { exact: true })).toHaveCount(0)
      const messages = await requester.api.get(`/api${humanPath}/messages`)
      expect(messages.items.map(({ body }: { body: string }) => body)).toEqual([publicReply])
      if (!process.env.WORKFLOW_BASE_URL) {
        await expect.poll(async () => (await api.get('/api/admin/notifications')).sent).toBeGreaterThan(sentBefore)
      }

      await button(staff, 'Wait for requester').click()
      await expect(staff.locator('.badge[data-status]')).toHaveText('Waiting for requester')
      await button(staff, 'Resume work').click()
      for (const [from, to] of [[normal, urgent], [urgent, normal]]) {
        await staff.getByLabel('New priority').selectOption(to.id)
        await button(staff, 'Save priority').click()
        await expect(staff.getByText(`${from.name} → ${to.name}`)).toBeVisible()
      }
      await propose(staff, 'en', 'Repaired the initial profile.')
      await open(owner, humanPath)
      await button(owner, 'Still not solved').click()
      await expect(status(owner, 'in_progress')).toBeVisible()
      await expect(fact(owner, 'Handling team')).toHaveText(team.name)
      await open(staff, staffPath)
      await propose(staff, 'en', 'Repaired the backup profile as well.')
      await open(owner, humanPath)
      await button(owner, labels.en.confirm).click()
      await expect(status(owner, 'closed')).toBeVisible()
      await expect(owner.getByText('Confirmed by the requester', { exact: true })).toBeVisible()
      await expect(button(owner, 'Reopen ticket')).toHaveCount(0)
      expect(await report()).toMatchObject({
        submissions: 1,
        operator: { initial_requester_confirmed: 1, initial_confirmation_timeout: 0, rejected_proposals: 1 },
      })
    })

    let solvedPath = ''
    await test.step('real provider and policy produce one final AI solution for English intake', async () => {
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
      const subject = 'English AI-only support request'
      const path = await submit(owner, subject, 'en', true)
      solvedPath = path
      await expect(owner.getByRole('heading', { name: 'AI-generated suggestion' })).toBeVisible()
      const suggestion = owner.getByTestId('ai-suggestion')
      await expect(suggestion).toHaveAttribute('lang', 'en')
      await expect(suggestion.getByRole('listitem')).toHaveText(['Close the application.', 'Open it again.'])
      await owner.getByLabel('Language', { exact: true }).selectOption('de')
      await expect.poll(async () => (await requester.api.get('/api/auth/me')).language).toBe('de')
      await expect(heading(owner)).toContainText(subject)
      await expect(owner.locator('html')).toHaveAttribute('lang', 'de')
      await expect(suggestion).toHaveAttribute('lang', 'en')
      await expect(owner.locator('section.ai-step').getByRole('button')).toHaveText(['Gelöst', 'Ich brauche weiterhin Hilfe'])
      await button(owner, 'Gelöst').dblclick()
      await expect(status(owner, 'ai_solved')).toBeVisible()
      await owner.reload()
      await expect(status(owner, 'ai_solved')).toBeVisible()
      await expect(owner.locator('html')).toHaveAttribute('lang', 'de')
      await expect(owner.getByRole('main').getByRole('textbox')).toHaveCount(0)

      const id = path.split('/').at(-1)
      const queuePath = '/api/queue?view=general'
      expect((await operator.api.get(queuePath)).items).toEqual([])
      await open(operator.page, '/queue?view=general')
      await expect(operator.page.getByRole('link', { name: subject })).toHaveCount(0)
      expect((await operator.api.request('GET', `/api${path}`)).status()).toBe(404)
      expect((await german.api.request('GET', `/api${path}`)).status()).toBe(404)
      await open(admin, `/admin/submissions/${id}`)
      await expect(heading(admin)).toContainText(subject)
      await expect(admin.getByText(/Read-only record/)).toBeVisible()
      expect(await report()).toMatchObject({ submissions: 2, ai_attempts: 1, ai_rate_numerator: 1 })
      expect(await api.get('/api/settings/ai/usage')).toMatchObject({ totals: { requests: 1, succeeded: 1 } })
    })

    await test.step('German AI handoff enters the same human workflow and survives a policy change', async () => {
      const owner = german.page
      const subject = 'Deutsche Anfrage mit KI-Vorschlag'
      const path = await submit(owner, subject, 'de', true)
      await expect(owner.getByRole('heading', { name: 'KI-generierter Vorschlag' })).toBeVisible()
      const suggestion = owner.getByTestId('ai-suggestion')
      await expect(suggestion).toHaveAttribute('lang', 'de')
      await expect(suggestion.getByRole('listitem')).toHaveText(['Anwendung schließen.', 'Anwendung neu öffnen.'])
      await button(owner, 'Ich brauche weiterhin Hilfe').click()
      await expect(status(owner, 'queued')).toBeVisible()

      const id = path.split('/').at(-1)
      expect((await operator.api.get('/api/queue?view=general')).items.map(({ id }: { id: string }) => id)).toEqual([id])
      const staff = specialist.page
      await open(staff, `/queue/${id}`)
      await staff.getByLabel('Zielteam').selectOption({ label: team.name })
      await button(staff, 'An Team senden').click()
      await expect(fact(staff, 'Team')).toHaveText(team.name)
      await button(staff, 'Ticket übernehmen').click()
      await expect(fact(staff, 'Zugewiesen an')).toHaveText(specialist.identity.username)
      await button(staff, 'Bearbeitung beginnen').click()
      await expect(staff.locator('.badge[data-status]')).toHaveText('In Bearbeitung')
      await propose(staff, 'de', 'Das Profil wurde wiederhergestellt.')
      await open(owner, path)
      await button(owner, labels.de.confirm).click()
      await expect(status(owner, 'closed')).toBeVisible()
      await expect(owner.getByText('Von der anfragenden Person bestätigt', { exact: true })).toBeVisible()
      await expect.poll(async () => (await german.api.get('/api/notifications')).items).toContainEqual(
        expect.objectContaining({
          ticket_id: id, href: path, title: 'Lösung vorgeschlagen: im Portal bestätigen oder ablehnen',
        }),
      )

      const settings = secondAdmin.page
      await open(settings, '/admin/settings')
      const toggle = settings.getByLabel('Erneutes Öffnen menschlich geschlossener Tickets erlauben')
      await expect(toggle).not.toBeChecked()
      await toggle.check()
      const saved = settings.waitForResponse((r) => r.url().endsWith('/api/admin/settings') && r.request().method() === 'PUT')
      await button(settings, 'Speichern').click()
      expect((await saved).ok()).toBe(true)
      await open(owner, path)
      await button(owner, 'Ticket wiedereröffnen').click()
      await expect(status(owner, 'in_progress')).toBeVisible()
      expect(await report()).toMatchObject({
        submissions: 3, ai_attempts: 2, ai_rate_numerator: 1,
        operator: { initial_requester_confirmed: 2, reopens: 1 },
      })
      const refused = await requester.api.request('POST', `/api${solvedPath}/actions`, { action: 'reopen' })
      expect(refused.ok()).toBe(false)
      await open(requester.page, solvedPath)
      await expect(status(requester.page, 'ai_solved')).toBeVisible()
      await expect(button(requester.page, 'Ticket wiedereröffnen')).toHaveCount(0)
      await open(requester.page, humanPath)
      await expect(status(requester.page, 'closed')).toBeVisible()
    })
  } finally {
    await Promise.all(contexts.map((context) => context.close()))
  }
})
