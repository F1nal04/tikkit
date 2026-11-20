import { createRequire } from 'node:module'
import { Client, assert, base, percentiles, settings } from './client.mjs'

const require = createRequire(new URL('../../apps/web/package.json', import.meta.url))
const { chromium } = require('@playwright/test')
const { password } = settings()
const operator = await new Client().login('load-operator-1', password, null)
const browser = await chromium.launch({ headless: true })
const records = []
try {
  for (let n = 0; n < 7; n++) {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, ignoreHTTPSErrors: true })
    try {
      const [name, ...pieces] = operator.cookie.split('=')
      await context.addCookies([{ name, value: pieces.join('='), url: base }])
      const page = await context.newPage()
      await page.addInitScript(() => {
        window.latestLcp = 0
        window.eventTimings = []
        new PerformanceObserver(list => {
          for (const entry of list.getEntries()) window.latestLcp = entry.startTime
        }).observe({ type: 'largest-contentful-paint', buffered: true })
        new PerformanceObserver(list => {
          for (const entry of list.getEntries()) {
            if (entry.interactionId) window.eventTimings.push(entry.duration)
          }
        }).observe({ type: 'event', buffered: true, durationThreshold: 16 })
      })
      const response = await page.goto(base + '/queue?view=general&lang=en', { waitUntil: 'networkidle' })
      assert.equal(response.status(), 200)
      const heading = page.getByRole('heading', { level: 1 })
      assert.equal(await heading.textContent(), 'General queue')
      // Mark first input through the next animation frame following a paint.
      const checkbox = page.getByRole('table').getByRole('checkbox').first()
      await page.evaluate(() => {
        document.addEventListener('click', event => {
          requestAnimationFrame(() => requestAnimationFrame(now => {
            window.clickToPaint = now - event.timeStamp
          }))
        }, { once: true, capture: true })
      })
      await checkbox.click()
      await page.waitForFunction(() => Number.isFinite(window.clickToPaint))
      const cold = await page.evaluate(() => {
        const nav = performance.getEntriesByType('navigation')[0]
        const paints = performance.getEntriesByType('paint')
        const js = performance.getEntriesByType('resource').filter(resource => new URL(resource.name).pathname.endsWith('.js'))
        return {
          ttfb_ms: nav.responseStart - nav.startTime,
          fcp_ms: paints.find(entry => entry.name === 'first-contentful-paint')?.startTime,
          lcp_ms: window.latestLcp,
          js_transfer_bytes: js.reduce((sum, resource) => sum + resource.transferSize, 0),
          js_encoded_bytes: js.reduce((sum, resource) => sum + resource.encodedBodySize, 0),
          click_to_paint_ms: window.clickToPaint,
          interaction_timing_ms: window.eventTimings.length ? Math.max(...window.eventTimings) : null,
        }
      })
      assert(cold.fcp_ms > 0 && cold.lcp_ms > 0 && cold.js_transfer_bytes > 0)
      const link = page.getByRole('table').getByRole('link').first()
      // The click and the render marker share the renderer's monotonic clock.
      await page.evaluate(() => {
        document.addEventListener('click', () => { window.navigationClick = performance.now() }, {
          once: true, capture: true,
        })
      })
      const subject = await link.textContent()
      await link.click()
      await page.waitForFunction(text => document.querySelector('h1')?.textContent.includes(text), subject)
      const queueToDetail = await page.evaluate(() => performance.now() - window.navigationClick)
      assert(Number.isFinite(queueToDetail), 'queue-to-detail left the document instead of using client navigation')
      const back = performance.now()
      await page.goBack()
      await page.waitForFunction(() => document.querySelector('h1')?.textContent === 'General queue')
      records.push({ ...cold, queue_to_detail_ms: queueToDetail, detail_to_queue_ms: performance.now() - back })
    } finally {
      await context.close()
    }
  }
  console.log(JSON.stringify({
    browser: browser.version(), contexts: records.length, throttling: 'none', viewport: '1440x900',
    percentiles: Object.fromEntries(Object.keys(records[0]).map(key => [key, percentiles(records.map(row => row[key]).filter(value => value !== null))])),
  }, null, 2))
} finally {
  await browser.close()
}
