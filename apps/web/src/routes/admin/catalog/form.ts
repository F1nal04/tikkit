/** Request body for creating or updating a conditional field from its form.
 * Existing choices are `choice_id`/`choice_label` pairs with `choice_archived`
 * checkboxes; `new_choices` adds one choice per line. */
export function fieldBody(form: FormData) {
  const archived = new Set(form.getAll('choice_archived').map(String))
  const labels = form.getAll('choice_label').map(String)
  const added = String(form.get('new_choices') ?? '')
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
  return {
    label: String(form.get('label') ?? ''),
    guidance: String(form.get('guidance') ?? ''),
    required: form.has('required'),
    allow_other: form.has('allow_other'),
    category_ids: form.getAll('category_ids').map(String),
    choices: [
      ...form.getAll('choice_id').map((v, i) => ({ id: String(v), label: labels[i], archived: archived.has(String(v)) })),
      ...added.map((label) => ({ label })),
    ],
  }
}
