import { tick } from 'svelte'

/** The form-error summary. One visible summary per page. */
export const FORM_ERRORS_ID = 'form-errors'

export type SummaryItem = { id: string; label: string; text: string }

/** One link per failing field. `ids` maps an API field key to the control's id when they differ. */
export function fieldItems(
  fields: Record<string, string[] | undefined> | undefined,
  labels: Record<string, string>,
  text: (code: string | undefined) => string | undefined,
  ids: Record<string, string> = {},
): SummaryItem[] {
  if (!fields) return []
  return Object.entries(fields).flatMap(([key, codes]) => {
    const message = text(codes?.[0])
    if (!message) return []
    return [{ id: ids[key] ?? key, label: labels[key] ?? key, text: message }]
  })
}

/** Move focus to the error summary after the failed submit has rendered it. */
export async function focusFormErrors() {
  await tick()
  document.getElementById(FORM_ERRORS_ID)?.focus()
}
