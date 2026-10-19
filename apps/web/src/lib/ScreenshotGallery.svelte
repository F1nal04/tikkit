<script lang="ts">
  import { messages, type Locale } from '#lib/i18n.ts'
  let { ids, locale }: { ids: string[]; locale: Locale } = $props()
  let h = $derived(messages[locale].history)
</script>
{#if ids.length}
  <ul class="images" aria-label={messages[locale].screenshots.label}>
    {#each ids as id, i (id)}
      <li><img src="/api/screenshots/{id}/preview" alt="{h.screenshot} {i + 1}" loading="lazy" /><a href="/api/screenshots/{id}/download">{h.download} {i + 1}</a></li>
    {/each}
  </ul>
{/if}
<style>
  .images { display: flex; flex-wrap: wrap; gap: 1rem; padding: 0; list-style: none; }
  li { display: grid; gap: .5rem; max-width: 100%; }
  img { max-width: 100%; width: 12rem; height: 9rem; object-fit: contain; border: 1px solid var(--line); }
</style>
