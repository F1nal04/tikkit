<script lang="ts">
  import type { Snippet } from 'svelte'

  let {
  kind = 'error',
  /** A form-error summary: focusable, so a failed submit can move focus here. */
  summary = false,
  children,
}: { kind?: 'error' | 'info' | 'success'; summary?: boolean; children: Snippet } = $props()
</script>

<!-- The icon is CSS generated content, so the text stays the accessible name and colour is never the only cue. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div
  class="alert {kind}"
  role={kind === 'error' ? 'alert' : 'status'}
  id={summary ? 'form-errors' : undefined}
  tabindex={summary ? -1 : undefined}
>{@render children()}</div>
