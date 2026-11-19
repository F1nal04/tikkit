import { tick } from 'svelte'

/** The form-error summary (`Alert` with `summary`). One per page. */
export const FORM_ERRORS_ID = 'form-errors'

/** Move focus to the error summary after the failed submit has rendered it. */
export async function focusFormErrors() {
  await tick()
  document.getElementById(FORM_ERRORS_ID)?.focus()
}
