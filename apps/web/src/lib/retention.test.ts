import { describe, expect, it, vi } from 'vitest'
import { ApiError } from '@tikkit/api-client'
import { exportPersonalData, keepsEverything, periodForm, policyOf, samePolicy, totalOf } from './retention.ts'

const keep = { draft_days: null, screenshot_days: null, ai_content_days: null, ticket_content_days: null, audit_days: null }

describe('retention periods', () => {
  it('shows "keep" as an empty input and reads it back as null', () => {
    const form = periodForm({ ...keep, draft_days: 30 })
    expect(form).toEqual({ draft_days: '30', screenshot_days: '', ai_content_days: '', ticket_content_days: '', audit_days: '' })
    expect(policyOf(form)).toEqual({ ...keep, draft_days: 30 })
    expect(keepsEverything(policyOf(periodForm(keep)))).toBe(true)
    expect(keepsEverything({ ...keep, audit_days: 90 })).toBe(false)
  })

  it('never turns a typo into "keep"', () => {
    const form = { ...periodForm(keep), draft_days: ' 45 ', screenshot_days: 'abc', ai_content_days: '-3', ticket_content_days: '1.5', audit_days: '1e3' }
    // 0 is out of range for the API, so the save fails with a field error.
    expect(policyOf(form)).toEqual({ draft_days: 45, screenshot_days: 0, ai_content_days: 0, ticket_content_days: 0, audit_days: 0 })
  })

  it('compares policies and sums counts', () => {
    expect(samePolicy(keep, { ...keep })).toBe(true)
    expect(samePolicy(keep, { ...keep, audit_days: 30 })).toBe(false)
    expect(totalOf({ drafts: 1, screenshots: 2, ai_content: 3, ticket_content: 4, audit_events: 5 })).toBe(15)
  })
})

describe('personal data export', () => {
  it('posts the typed username and returns the attachment', async () => {
    const fetchImpl = vi.fn(async () => new Response('{"format":"x"}', { status: 200, headers: { 'content-disposition': 'attachment; filename="personal-data-a1.json"' } }))
    const result = await exportPersonalData('a1', 'requester', fetchImpl as unknown as typeof fetch)
    expect(result.filename).toBe('personal-data-a1.json')
    expect(await result.blob.text()).toBe('{"format":"x"}')
    const [url, init] = fetchImpl.mock.calls[0] as unknown as [string, RequestInit]
    expect(url).toBe('/api/admin/accounts/a1/personal-data/export')
    expect(init.method).toBe('POST')
    expect(init.body).toBe('{"username":"requester"}')
    expect((init.headers as Record<string, string>)['x-requested-with']).toBe('tikkit')
  })

  it('surfaces the API error with its field codes', async () => {
    const envelope = { error: { code: 'validation_failed', message: 'x', details: { fields: { username: ['mismatch'] } } }, request_id: 'r' }
    const fetchImpl = (async () => new Response(JSON.stringify(envelope), { status: 422 })) as unknown as typeof fetch
    const error = await exportPersonalData('a1', 'someone', fetchImpl).catch((e) => e)
    expect(error).toBeInstanceOf(ApiError)
    expect(error.fields).toEqual({ username: ['mismatch'] })
  })
})
