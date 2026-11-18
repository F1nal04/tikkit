import { expect, test } from '@playwright/test'
import { signIn } from './support'

// The production build's own boundary (docs/security.md): the CSP SvelteKit
// renders with a per-response nonce, the other page headers, and SvelteKit's
// origin check for form posts. API responses are covered by the Rust tests.

test('pages carry a nonce-based CSP and the other security headers', async ({ request }) => {
  const first = await request.get('/login')
  const second = await request.get('/login')
  const csp = first.headers()['content-security-policy']
  expect(csp).toBeTruthy()
  const directives = Object.fromEntries(
    csp.split(';').map((d) => d.trim().split(/\s+/)).map(([name, ...values]) => [name, values]),
  )
  expect(directives['default-src']).toEqual(["'self'"])
  expect(directives['frame-ancestors']).toEqual(["'none'"])
  expect(directives['object-src']).toEqual(["'none'"])
  expect(directives['base-uri']).toEqual(["'none'"])
  expect(directives['form-action']).toEqual(["'self'"])
  const scripts = directives['script-src'].join(' ')
  expect(scripts).toMatch(/'nonce-[^']+'/)
  expect(scripts).not.toContain('unsafe-inline')
  expect(scripts).not.toContain('unsafe-eval')
  expect(directives['style-src'].join(' ')).not.toContain('unsafe-inline')
  // Only style attributes (SvelteKit's route announcer) may be inline.
  expect(directives['style-src-attr']).toEqual(["'unsafe-inline'"])
  // A fresh nonce for every response.
  expect(second.headers()['content-security-policy']).not.toBe(csp)
  const headers = first.headers()
  expect(headers['x-content-type-options']).toBe('nosniff')
  expect(headers['x-frame-options']).toBe('DENY')
  expect(headers['referrer-policy']).toBe('no-referrer')
  expect(headers['cross-origin-opener-policy']).toBe('same-origin')
  expect(headers['permissions-policy']).toContain('camera=()')
  expect(headers['cache-control']).toBe('private, no-store')
  // adapter-node assumes https://<Host> without PROTOCOL_HEADER, which proves
  // nothing about the connection: no HSTS (the test servers set no header).
  expect(headers['strict-transport-security']).toBeUndefined()
})

test('the app hydrates and works under the CSP without a single violation', async ({ page }) => {
  const violations: string[] = []
  page.on('console', (message) => {
    if (/Content Security Policy/i.test(message.text())) violations.push(message.text())
  })
  await page.addInitScript(() => {
    document.addEventListener('securitypolicyviolation', (event) => {
      const list = ((window as unknown as { cspViolations?: string[] }).cspViolations ??= [])
      list.push(`${event.violatedDirective} ${event.blockedURI}`)
    })
  })
  // Signing in, a ticket page with stored content and a screenshot, and client navigation.
  await signIn(page, 'requester')
  const ids = await page.evaluate(async () => {
    const r = await fetch('/api/test/history', {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' },
      body: '{}',
    })
    return (await r.json()) as Record<string, string>
  })
  await page.goto(`/tickets/${ids.queued}`)
  await page.waitForLoadState('networkidle')
  await expect(page.getByRole('heading', { name: 'Original submission' })).toBeVisible()
  await expect(page.getByRole('article').first()).toBeVisible()
  await page.goto('/')
  await page.getByRole('link', { name: 'requester: AI solved final' }).click()
  await expect(page).toHaveURL(/\/tickets\//)
  const recorded = await page.evaluate(
    () => (window as unknown as { cspViolations?: string[] }).cspViolations ?? [],
  )
  expect([...violations, ...recorded]).toEqual([])
})

test('cross-site form posts to the portal are refused', async ({ request, baseURL }) => {
  for (const contentType of ['application/x-www-form-urlencoded', 'multipart/form-data; boundary=x', 'text/plain']) {
    const response = await request.post('/login', {
      headers: { origin: 'https://evil.example', 'content-type': contentType },
      data: 'username=admin&password=x',
    })
    expect(response.status(), contentType).toBe(403)
  }
  // The portal's own origin (https://<Host>, see above) passes the check and
  // then finds no form action; plain http is a different origin.
  const own = baseURL!.replace(/^http:/, 'https:')
  const plain = await request.post('/login', {
    headers: { origin: baseURL!, 'content-type': 'application/x-www-form-urlencoded' },
    data: 'username=admin&password=x',
  })
  expect(plain.status()).toBe(403)
  const same = await request.post('/login', {
    headers: { origin: own, 'content-type': 'application/x-www-form-urlencoded' },
    data: 'username=admin&password=x',
  })
  expect(same.status()).not.toBe(403)
})
