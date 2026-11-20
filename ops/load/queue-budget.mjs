import { readFileSync } from 'node:fs'
import { get } from 'node:https'
import { gzipSync } from 'node:zlib'
import { Client, assert, base, settings, work } from './client.mjs'

const ca = readFileSync(work + '/portal-ca.crt')
const { password } = settings()
const actor = await new Client().login('load-operator-1', password, null)
const page = await fetch(base + '/queue?view=general&lang=en', {
  headers: { Cookie: actor.cookie }, redirect: 'manual', tls: { ca },
})
assert.equal(page.status, 200, 'signed-in SSR queue')
const html = await page.text()
const urls = new Set()
const add = path => {
  const url = new URL(path, base)
  assert.equal(url.origin, base, 'same-origin JS')
  if (url.pathname.endsWith('.js')) urls.add(url.href)
}
for (const tag of html.matchAll(/<link\b[^>]*>/gi)) {
  if (/\brel=["']modulepreload["']/i.test(tag[0])) {
    const href = /\bhref=["']([^"']+)["']/i.exec(tag[0])
    assert(href, 'SSR preload has href')
    add(href[1])
  }
}
for (const [, path] of (page.headers.get('link') ?? '').matchAll(/<([^>]+)>;[^,]*\brel="?modulepreload"?/gi)) add(path)
assert(urls.size > 0, 'SSR queue modulepreload list is empty')
// Adapter bootstrap imports can be inline even when the preload list exists.
for (const [, path] of html.matchAll(/\bimport\(["']([^"']+\.js)["']\)/g)) add(path)

function transfer(url) {
  return new Promise((resolve, reject) => {
    get(url, { ca, headers: { 'Accept-Encoding': 'gzip' } }, response => {
      if (response.statusCode !== 200) {
        response.resume()
        reject(new Error('SSR JavaScript request failed'))
        return
      }
      const chunks = []
      response.on('data', chunk => chunks.push(chunk))
      response.on('error', reject)
      response.on('end', () => {
        const body = Buffer.concat(chunks)
        const encoding = response.headers['content-encoding']
        if (encoding && encoding !== 'gzip') reject(new Error('Unexpected JavaScript encoding'))
        else resolve(encoding === 'gzip' ? body.length : gzipSync(body, { level: 9 }).length)
      })
    }).on('error', reject).setTimeout(10_000, function () { this.destroy(new Error('JavaScript download timed out')) })
  })
}

const gzip = (await Promise.all([...urls].map(transfer))).reduce((sum, bytes) => sum + bytes, 0)
const budget = 100 * 1024
console.log(JSON.stringify({ route: '/queue', files: urls.size, gzip_bytes: gzip, budget_bytes: budget }))
assert(gzip <= budget, 'SSR queue initial JavaScript budget exceeded')
