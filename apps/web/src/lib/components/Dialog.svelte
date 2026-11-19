<script lang="ts">
  import type { Snippet } from 'svelte'
  import { translate, type Locale } from '#lib/i18n.ts'

  type Props = { open?: boolean; title: string; locale: Locale; children: Snippet; actions?: Snippet }
  let { open = $bindable(false), title, locale, children, actions }: Props = $props()
  const id = $props.id()
  let dialog: HTMLDialogElement
  // Outside the dialog, so the close announcement stays in the tree after the dialog hides.
  let announcement = $state('')
  let wasOpen = false

  // Native modal dialog: focus is trapped, Escape closes, and focus returns to the opener.
  $effect(() => {
    if (open && !dialog.open) dialog.showModal()
    else if (!open && dialog.open) dialog.close()
  })
  // A native modal already announces itself. Only the close is spoken from outside the dialog,
  // after the dialog has left the accessibility tree. The closed state on first render says nothing.
  $effect(() => {
    const now = open
    if (now === wasOpen) return
    if (!now) announcement = translate(locale, 'dialogClosed')
    wasOpen = now
  })
</script>

<p class="sr-only" role="status" data-testid="dialog-announcement">{announcement}</p>
<!-- A click on the backdrop lands on the dialog element itself; Escape covers the keyboard. -->
<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
<dialog
  bind:this={dialog}
  aria-labelledby="{id}-title"
  onclose={() => (open = false)}
  onclick={(event) => event.target === dialog && (open = false)}
>
  <h2 id="{id}-title">{title}</h2>
  {@render children()}
  {#if actions}<div class="actions">{@render actions()}</div>{/if}
</dialog>
