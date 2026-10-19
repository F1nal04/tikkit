<script lang="ts">
  import type { Snippet } from 'svelte'

  type Props = { open?: boolean; title: string; children: Snippet; actions?: Snippet }
  let { open = $bindable(false), title, children, actions }: Props = $props()
  const id = $props.id()
  let dialog: HTMLDialogElement

  // Native modal dialog: focus is trapped, Escape closes, and focus returns to the opener.
  $effect(() => {
    if (open && !dialog.open) dialog.showModal()
    else if (!open && dialog.open) dialog.close()
  })
</script>

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
