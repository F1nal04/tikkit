<script lang="ts">
  import type { Snippet } from 'svelte'
  import EmptyState from './EmptyState.svelte'

  type Props = { caption: string; empty: string; isEmpty: boolean; head: Snippet; children: Snippet }
  let { caption, empty, isEmpty, head, children }: Props = $props()
</script>

{#if isEmpty}
  <EmptyState title={empty} />
{:else}
  <!-- A scrollable region must be focusable so keyboard users can reach wide tables on small screens. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div class="table-wrap" role="region" aria-label={caption} tabindex="0">
    <table>
      <caption class="sr-only">{caption}</caption>
      <thead>{@render head()}</thead>
      <tbody>{@render children()}</tbody>
    </table>
  </div>
{/if}
