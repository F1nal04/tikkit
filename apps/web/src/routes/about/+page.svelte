<script lang="ts">
  import { messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
</script>

<svelte:head>
  <title>{t.about}</title>
</svelte:head>

<main>
  <h1>{t.about}</h1>
  <p>{t.aboutText}</p>
  <a href="/">{t.home}</a>
</main>
