<script lang="ts">
  import type { SummaryItem } from '#lib/a11y.ts'

  let { title, items = [] }: { title: string; items?: SummaryItem[] } = $props()
</script>

<!-- Focus moves here after a failed submit, so this is not also an alert (that would announce twice). -->
<div id="form-errors" class="alert error" tabindex="-1">
  <p>{title}</p>
  {#if items.length}
    <ul>
      {#each items as item (item.id)}
        <li><a href="#{item.id}">{item.label}: {item.text}</a></li>
      {/each}
    </ul>
  {/if}
</div>
