<script lang="ts">
  import type { Snippet } from 'svelte'
  import { messages, type Locale } from '#lib/i18n.ts'
  import LanguageSelect from './LanguageSelect.svelte'

  let { locale, children }: { locale: Locale; children: Snippet } = $props()
  let t = $derived(messages[locale])
</script>

<header class="topbar">
  <a href="/" class="brand">Tikkit</a>
  <LanguageSelect {locale} user={null} />
</header>
<main id="main" tabindex="-1" class="narrow">{@render children()}</main>
