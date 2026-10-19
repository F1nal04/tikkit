<script lang="ts">
  import EmptyState from '#lib/components/EmptyState.svelte'
  import { messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
</script>

<svelte:head><title>{t.newTicketTitle}</title></svelte:head>

<h1>{t.newTicketTitle}</h1>
<EmptyState title={t.newTicketEmpty} />
