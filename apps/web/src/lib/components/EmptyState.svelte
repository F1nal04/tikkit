<script lang="ts">
  import type { Snippet } from 'svelte'

  let { title, children }: { title: string; children?: Snippet } = $props()
</script>

<div class="empty">
  <p class="empty-title">{title}</p>
  {#if children}<p>{@render children()}</p>{/if}
</div>
