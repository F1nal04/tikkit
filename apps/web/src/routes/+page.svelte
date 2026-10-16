<script lang="ts">
  import { messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
</script>

<svelte:head>
  <title>Tikkit</title>
  <meta name="description" content="Tikkit — SvelteKit and Rust." />
</svelte:head>

<main>
  <p class="eyebrow">{t.eyebrow}</p>
  <h1>Tikkit</h1>
  <p>{t.tagline}</p>
  <p class="status" role="status">{data.healthy ? t.apiHealthy : t.apiUnavailable}</p>
  <a href="/about">{t.about}</a>
</main>
