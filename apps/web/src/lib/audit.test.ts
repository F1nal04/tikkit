import { expect, test, vi } from 'vitest'
import { ApiError } from '@tikkit/api-client'
import { auditFilters, auditHref, auditQuery, exportAudit, hasMetadata, metadataText } from './audit.ts'

const parse = (query: string) => auditFilters(new URLSearchParams(query))
const EMPTY = { actor_username: '', action: '', ticket_reference: '', resource_type: '', resource_id: '', request_id: '', from: '', until: '', cursor: '' }

test('filters come from the address bar; values the API would reject fall back to none', () => {
  expect(parse('')).toEqual(EMPTY)
  expect(parse('actor_username=%20Olga%20&action=ticket_ai_solved&ticket_reference=%231042&resource_type=team&resource_id=t1&request_id=7D0C0A52-6F0E-4C1E-9A55-0D6F1F2B3C4D&from=2026-10-01&until=2026-10-31&cursor=c')).toEqual({
    actor_username: 'Olga', action: 'ticket_ai_solved', ticket_reference: '1042', resource_type: 'team', resource_id: 't1', request_id: '7d0c0a52-6f0e-4c1e-9a55-0d6f1f2b3c4d', from: '2026-10-01', until: '2026-10-31', cursor: 'c',
  })
  expect(parse(`action=Login%20Failed&ticket_reference=abc&resource_type=Team!&request_id=req-1&from=2026-02-30&until=soon`)).toEqual(EMPTY)
  expect(parse('ticket_reference=0')).toEqual(EMPTY)
  expect(parse('from=2026-10-31&until=2026-10-01')).toMatchObject({ from: '2026-10-01', until: '2026-10-31' })
})

test('the API query uses inclusive UTC days, a numeric reference, and the page size', () => {
  expect(auditQuery(parse('ticket_reference=1042&from=2026-10-01&until=2026-10-31&action=login_failed'))).toEqual({
    actor_username: undefined, action: 'login_failed', ticket_reference: 1042, resource_type: undefined, resource_id: undefined, request_id: undefined,
    created_from: '2026-10-01T00:00:00Z', created_before: '2026-11-01T00:00:00.000Z', cursor: undefined, limit: 50,
  })
})

test('links keep the filters and drop empty ones', () => {
  expect(auditHref({})).toBe('/admin/audit')
  expect(auditHref({ ...parse('action=logout&cursor=c1'), cursor: '' })).toBe('/admin/audit?action=logout')
  expect(auditHref({ resource_id: 'a b' })).toBe('/admin/audit?resource_id=a+b')
})

test('metadata is rendered as plain JSON text', () => {
  expect(hasMetadata({})).toBe(false)
  expect(hasMetadata({ a: 1 })).toBe(true)
  expect(metadataText({ from: { allow_reopening: false }, note: '<b>x</b>' })).toBe('{\n  "from": {\n    "allow_reopening": false\n  },\n  "note": "<b>x</b>"\n}')
})

test('the export posts the filters without paging and reports the server bounds', async () => {
  const fetchImpl = vi.fn(async () => new Response('"id"\r\n', {
    headers: { 'content-disposition': 'attachment; filename="tikkit-audit-2026-11-18.csv"', 'x-export-rows': '10000', 'x-export-truncated': 'true' },
  }))
  const result = await exportAudit(parse('action=login_failed&ticket_reference=7&cursor=c1'), fetchImpl as unknown as typeof fetch)
  expect(fetchImpl).toHaveBeenCalledWith('/api/admin/audit/export?action=login_failed&ticket_reference=7', {
    method: 'POST', headers: { 'x-requested-with': 'tikkit' }, credentials: 'include',
  })
  expect(result).toMatchObject({ filename: 'tikkit-audit-2026-11-18.csv', rows: 10000, truncated: true })
  expect(await result.blob.text()).toBe('"id"\r\n')
})

test('a refused export surfaces the API error code', async () => {
  const refused = async () => new Response(JSON.stringify({ error: { code: 'forbidden', message: 'no' }, request_id: 'r' }), { status: 403 })
  await expect(exportAudit(parse(''), refused as unknown as typeof fetch)).rejects.toMatchObject({ status: 403, code: 'forbidden' })
  const offline = async () => { throw new TypeError('offline') }
  const error = await exportAudit(parse(''), offline as unknown as typeof fetch).catch((e) => e)
  expect(error).toBeInstanceOf(ApiError)
  expect(error.code).toBe('unavailable')
})
