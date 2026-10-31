<script lang="ts">
  import { messages } from '#lib/i18n.ts'
  import { NAV } from '#lib/nav.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  // Whatever the sidebar lists under Administration, so new sections show up without touching this page.
  const sections = NAV.filter((item) => item.section === 'admin' && item.href !== '/admin')
</script>

<svelte:head><title>{t.adminHomeTitle}</title></svelte:head>

<h1>{t.adminHomeTitle}</h1>
<p>{t.adminHomeIntro}</p>
<ul>
  {#each sections as item (item.href)}<li><a href={item.href}>{t.nav[item.key]}</a></li>{/each}
</ul>
