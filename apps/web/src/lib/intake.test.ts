import { describe, expect, it } from 'vitest'
import {
  attemptFor,
  emptyForm,
  hiddenAnswers,
  isEmpty,
  OTHER,
  restore,
  sameIntake,
  toIntake,
  uuid,
  validate,
  visibleFields,
  type Catalog,
} from './intake.ts'

const SOFTWARE = '11111111-1111-4111-8111-111111111111'
const NETWORK = '22222222-2222-4222-8222-222222222222'
const HIGH = '33333333-3333-4333-8333-333333333333'
const PROGRAM = '44444444-4444-4444-8444-444444444444'
const WORD = '55555555-5555-4555-8555-555555555555'
const NOTE = '66666666-6666-4666-8666-666666666666'

const catalog: Catalog = {
  categories: [
    { id: SOFTWARE, name: 'Software', skip_ai: false, archived: false },
    { id: NETWORK, name: 'Network', skip_ai: true, archived: false },
  ],
  priorities: [{ id: HIGH, name: 'High', archived: false }],
  fields: [
    {
      id: PROGRAM,
      label: 'Program',
      guidance: '',
      kind: 'select',
      required: true,
      allow_other: true,
      category_ids: [SOFTWARE],
      choices: [{ id: WORD, label: 'Word', archived: false }],
      archived: false,
    },
    {
      id: NOTE,
      label: 'Note',
      guidance: '',
      kind: 'text',
      required: false,
      allow_other: false,
      category_ids: [NETWORK],
      choices: [],
      archived: false,
    },
  ],
}

const filled = () => ({
  ...emptyForm(),
  category_id: SOFTWARE,
  priority_id: HIGH,
  subject: 'Word crashes',
  description: 'It closes on startup.',
})

describe('visibility', () => {
  it('reveals a field only for the categories that trigger it', () => {
    expect(visibleFields(catalog, SOFTWARE).map((f) => f.id)).toEqual([PROGRAM])
    expect(visibleFields(catalog, NETWORK).map((f) => f.id)).toEqual([NOTE])
    expect(visibleFields(catalog, '')).toEqual([])
  })

  it('requires a visible required field and ignores it once hidden', () => {
    expect(validate(filled(), catalog)[`answers.${PROGRAM}`]).toEqual(['required'])
    const hidden = { ...filled(), category_id: NETWORK }
    expect(validate(hidden, catalog)).toEqual({})
  })

  it('never submits a hidden or stale answer but remembers it', () => {
    const form = { ...filled(), answers: { [PROGRAM]: { choice: WORD, text: '' } } }
    expect(toIntake(form, catalog).answers).toEqual({ [PROGRAM]: { choice_id: WORD } })
    const moved = { ...form, category_id: NETWORK }
    expect(toIntake(moved, catalog).answers).toEqual({})
    expect(hiddenAnswers(catalog, moved).map((f) => f.id)).toEqual([PROGRAM])
    // Switching back restores the answer; nothing was lost.
    expect(toIntake({ ...moved, category_id: SOFTWARE }, catalog).answers).toEqual({ [PROGRAM]: { choice_id: WORD } })
  })

  it('sends free text for the alternative and rejects a choice that was archived', () => {
    const other = { ...filled(), answers: { [PROGRAM]: { choice: OTHER, text: 'Notepad++' } } }
    expect(toIntake(other, catalog).answers).toEqual({ [PROGRAM]: { text: 'Notepad++' } })
    expect(validate(other, catalog)).toEqual({})
    const gone = { ...filled(), answers: { [PROGRAM]: { choice: 'archived-choice', text: '' } } }
    expect(toIntake(gone, catalog).answers).toEqual({})
    expect(validate(gone, catalog)[`answers.${PROGRAM}`]).toEqual(['invalid'])
    expect(validate({ ...other, answers: { [PROGRAM]: { choice: OTHER, text: ' ' } } }, catalog)[`answers.${PROGRAM}`]).toEqual(['required'])
  })
})

describe('validation', () => {
  it('uses the server codes and keys', () => {
    expect(validate(emptyForm(), catalog)).toEqual({
      subject: ['required'],
      description: ['required'],
      category_id: ['required'],
      priority_id: ['required'],
    })
    expect(validate({ ...filled(), subject: 'x'.repeat(201), category_id: 'gone' }, catalog)).toMatchObject({
      subject: ['too_long'],
      category_id: ['invalid'],
    })
  })
})

describe('resuming a draft', () => {
  it('keeps text but empties an archived category or priority and says so', () => {
    const r = restore({ ...toIntake(filled(), catalog), category_id: 'archived', priority_id: 'archived' }, catalog)
    expect(r).toMatchObject({ categoryGone: true, priorityGone: true })
    expect(r.form).toMatchObject({ category_id: '', priority_id: '', subject: 'Word crashes' })
  })

  it('restores valid answers and reports the ones it cannot', () => {
    const r = restore(
      {
        ...toIntake(filled(), catalog),
        answers: { [PROGRAM]: { choice_id: 'archived-choice' }, 'unknown-field': { text: 'kept for the user to see' } },
      },
      catalog,
    )
    expect(r.form.answers).toEqual({})
    expect(r.dropped).toEqual([{ label: 'Program', text: undefined }, { label: undefined, text: 'kept for the user to see' }])
    const ok = restore({ ...toIntake(filled(), catalog), answers: { [PROGRAM]: { text: 'Notepad++' } } }, catalog)
    expect(ok.form.answers[PROGRAM]).toEqual({ choice: OTHER, text: 'Notepad++' })
    expect(ok.dropped).toEqual([])
  })

  it('sees a resumed draft as different when it still holds an answer the form dropped', () => {
    const visible = toIntake({ ...filled(), category_id: NETWORK }, catalog)
    const saved = { ...visible, answers: { [PROGRAM]: { choice_id: null, text: 'Lotus Notes' } } }
    expect(sameIntake(visible, saved)).toBe(false)
    // The server sends both answer parts; omitting a null one is the same intake.
    const choice = toIntake({ ...filled(), answers: { [PROGRAM]: { choice: WORD, text: '' } } }, catalog)
    expect(sameIntake(choice, { ...choice, answers: { [PROGRAM]: { choice_id: WORD, text: null } } })).toBe(true)
  })

  it('detects an untouched form so no empty draft is created', () => {
    expect(isEmpty(toIntake(emptyForm(), catalog))).toBe(true)
    expect(isEmpty(toIntake({ ...emptyForm(), subject: 'a' }, catalog))).toBe(false)
  })
})

describe('idempotency key', () => {
  it('is stable for a retry and fresh for changed input', () => {
    let n = 0
    const make = () => `key-${++n}`
    const first = attemptFor(null, 'a', make)
    expect(attemptFor(first, 'a', make)).toBe(first)
    expect(attemptFor(first, 'b', make).key).toBe('key-2')
  })

  it('generates v4 UUIDs without secure-context APIs', () => {
    expect(uuid()).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/)
  })
})
