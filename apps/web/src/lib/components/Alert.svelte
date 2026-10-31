<script lang="ts">
  import type { Snippet } from 'svelte'

  let { kind = 'error', children }: { kind?: 'error' | 'info' | 'success'; children: Snippet } = $props()
</script>

<!-- The icon is CSS generated content, so the text stays the accessible name and colour is never the only cue. -->
<div class="alert {kind}" role={kind === 'error' ? 'alert' : 'status'}>{@render children()}</div>
