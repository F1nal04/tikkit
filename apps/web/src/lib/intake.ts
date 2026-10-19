// Logic of the shared ticket form: which conditional fields are visible, what is sent,
// and what a resumed draft keeps. Convenience only: Rust validates everything again.
import type { Schemas } from '@tikkit/api-client'

export type Catalog = Schemas['Catalog']
export type IntakeField = Schemas['IntakeField']
export type Intake = Schemas['TicketIntake']

/** Select value for "another answer" on a field that allows free text. */
export const OTHER = '__other__'

// Server limits (docs/submission.md).
export const SUBJECT_MAX = 200
export const DESCRIPTION_MAX = 20_000
export const ANSWER_MAX = 500

/** One conditional answer as the inputs hold it: a choice ID (or `OTHER`) and/or typed text. */
export type Answer = { choice: string; text: string }

export type FormState = {
  category_id: string
  priority_id: string
  subject: string
  description: string
  /** Keyed by field ID. Kept for fields that are currently hidden, but never sent. */
  answers: Record<string, Answer>
  screenshot_ids: string[]
}

export const emptyForm = (): FormState => ({
  category_id: '',
  priority_id: '',
  subject: '',
  description: '',
  answers: {},
  screenshot_ids: [],
})

/** Fields that the selected category reveals. */
export const visibleFields = (catalog: Catalog, categoryId: string) =>
  categoryId ? catalog.fields.filter((f) => f.category_ids.includes(categoryId)) : []

const hasValue = (field: IntakeField, a: Answer | undefined) =>
  !!a && (field.kind === 'select' && a.choice !== OTHER ? !!a.choice : !!a.text.trim())

/** Answers the user typed for fields the category no longer reveals: not sent, but not forgotten. */
export const hiddenAnswers = (catalog: Catalog, form: FormState) =>
  catalog.fields.filter((f) => !visibleFields(catalog, form.category_id).includes(f) && hasValue(f, form.answers[f.id]))

/**
 * Whether two intakes would submit the same fields. Null and omitted answer parts compare equal,
 * and answer-key order does not matter, so a resumed draft can be told apart from the visible form.
 */
export function sameIntake(a: Intake, b: Intake): boolean {
  const norm = (intake: Intake) => {
    const answers: Record<string, { choice_id: string | null; text: string | null }> = {}
    for (const id of Object.keys(intake.answers ?? {}).sort()) {
      const answer = intake.answers?.[id]
      const choice_id = answer?.choice_id ?? null
      const text = answer?.text ?? null
      if (!choice_id && !text) continue
      answers[id] = { choice_id, text }
    }
    return JSON.stringify({
      category_id: intake.category_id ?? null,
      priority_id: intake.priority_id ?? null,
      subject: intake.subject ?? '',
      description: intake.description ?? '',
      answers,
      screenshot_ids: intake.screenshot_ids ?? [],
    })
  }
  return norm(a) === norm(b)
}

/** The request body: only visible fields' answers, so hidden or stale ones never leave the browser. */
export function toIntake(form: FormState, catalog: Catalog): Intake {
  const answers: Intake['answers'] = {}
  for (const field of visibleFields(catalog, form.category_id)) {
    const a = form.answers[field.id]
    if (!a) continue
    if (field.kind === 'select' && a.choice !== OTHER) {
      // A choice archived since the form loaded is dropped here; validation flags it as missing.
      if (field.choices.some((c) => c.id === a.choice)) answers[field.id] = { choice_id: a.choice }
    } else if (a.text.trim()) answers[field.id] = { text: a.text }
  }
  return {
    category_id: form.category_id || null,
    priority_id: form.priority_id || null,
    subject: form.subject,
    description: form.description,
    answers,
    screenshot_ids: form.screenshot_ids,
  }
}

export const isEmpty = (intake: Intake) =>
  !intake.category_id &&
  !intake.priority_id &&
  !intake.subject &&
  !intake.description &&
  !Object.keys(intake.answers ?? {}).length &&
  !intake.screenshot_ids?.length

/** What a resumed draft could not keep, so the form can say so instead of dropping it silently. */
export type Restored = {
  form: FormState
  categoryGone: boolean
  priorityGone: boolean
  dropped: { label?: string; text?: string }[]
}

/** Fits a saved draft to today's catalog. Archived choices become empty fields, never stale selections. */
export function restore(intake: Intake, catalog: Catalog): Restored {
  const form = emptyForm()
  form.subject = intake.subject ?? ''
  form.description = intake.description ?? ''
  form.screenshot_ids = intake.screenshot_ids ?? []
  const categoryGone = !!intake.category_id && !catalog.categories.some((c) => c.id === intake.category_id)
  const priorityGone = !!intake.priority_id && !catalog.priorities.some((p) => p.id === intake.priority_id)
  if (!categoryGone) form.category_id = intake.category_id ?? ''
  if (!priorityGone) form.priority_id = intake.priority_id ?? ''

  const visible = new Map(visibleFields(catalog, form.category_id).map((f) => [f.id, f]))
  const dropped: Restored['dropped'] = []
  for (const [id, saved] of Object.entries(intake.answers ?? {})) {
    const field = visible.get(id)
    const text = saved.text?.trim() || undefined
    if (field && saved.choice_id && field.choices.some((c) => c.id === saved.choice_id)) {
      form.answers[id] = { choice: saved.choice_id, text: '' }
    } else if (field && !saved.choice_id && text && (field.kind === 'text' || field.allow_other)) {
      form.answers[id] = { choice: field.kind === 'select' ? OTHER : '', text }
    } else if (saved.choice_id || text) {
      dropped.push({ label: catalog.fields.find((f) => f.id === id)?.label, text })
    }
  }
  return { form, categoryGone, priorityGone, dropped }
}

/** Client-side mirror of the server rules, keyed like the API's field errors. */
export function validate(form: FormState, catalog: Catalog): Record<string, string[]> {
  const errors: Record<string, string[]> = {}
  const fail = (key: string, code: string) => (errors[key] = [code])
  if (!form.subject.trim()) fail('subject', 'required')
  else if (form.subject.length > SUBJECT_MAX) fail('subject', 'too_long')
  if (!form.description.trim()) fail('description', 'required')
  else if (form.description.length > DESCRIPTION_MAX) fail('description', 'too_long')
  if (!form.category_id) fail('category_id', 'required')
  else if (!catalog.categories.some((c) => c.id === form.category_id)) fail('category_id', 'invalid')
  if (!form.priority_id) fail('priority_id', 'required')
  else if (!catalog.priorities.some((p) => p.id === form.priority_id)) fail('priority_id', 'invalid')
  const sent = toIntake(form, catalog).answers ?? {}
  for (const field of visibleFields(catalog, form.category_id)) {
    const key = `answers.${field.id}`
    const a = form.answers[field.id]
    if (a && field.kind === 'select' && a.choice && a.choice !== OTHER && !sent[field.id]) fail(key, 'invalid')
    else if (field.required && !sent[field.id]) fail(key, 'required')
    else if ((a?.text.length ?? 0) > ANSWER_MAX) fail(key, 'too_long')
  }
  return errors
}

/**
 * One idempotency key per logical submission: a retry of the same request reuses it (the server
 * then returns the original receipt), changed input gets a new one (the server refuses to
 * pair a key with different input).
 */
export type Attempt = { fingerprint: string; key: string }
export function attemptFor(previous: Attempt | null, fingerprint: string, makeKey = uuid): Attempt {
  return previous?.fingerprint === fingerprint ? previous : { fingerprint, key: makeKey() }
}

// `crypto.randomUUID` needs a secure context; a deployment may serve plain HTTP on a LAN.
export function uuid(): string {
  const b = crypto.getRandomValues(new Uint8Array(16))
  b[6] = (b[6] & 0x0f) | 0x40
  b[8] = (b[8] & 0x3f) | 0x80
  const h = [...b].map((x) => x.toString(16).padStart(2, '0')).join('')
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`
}
