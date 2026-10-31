<script lang="ts">
  import { messages, type Locale } from '#lib/i18n.ts'

  let { status, locale }: { status: number; locale: Locale } = $props()
  let t = $derived(messages[locale])
  // Every denial reads the same, so a hidden page's existence is not revealed.
  let kind = $derived<'forbidden' | 'notFound' | 'generic'>(status === 403 ? 'forbidden' : status === 404 ? 'notFound' : 'generic')
</script>

<svelte:head><title>{t.errorPages[kind].title}</title></svelte:head>

<h1>{t.errorPages[kind].title}</h1>
<p>{t.errorPages[kind].text}</p>
<p><a href="/">{t.home}</a></p>
