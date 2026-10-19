<script lang="ts">
  import { page } from '$app/state'
  import { messages } from '#lib/i18n.ts'
  import type { LayoutProps } from './$types'
  let { data, children }: LayoutProps = $props()
  let t = $derived(messages[data.locale])
</script>
<nav aria-label={t.administration}>
  <a href="/admin/accounts" aria-current={page.url.pathname === '/admin/accounts' ? 'page' : undefined}>{t.accountsTitle}</a>
  <a href="/admin/teams" aria-current={page.url.pathname === '/admin/teams' ? 'page' : undefined}>{t.teamsTitle}</a>
  <a href="/admin/settings" aria-current={page.url.pathname === '/admin/settings' ? 'page' : undefined}>{t.settingsTitle}</a>
</nav>
{@render children()}
<style>
  nav { display: flex; flex-wrap: wrap; gap: 1.5rem; padding: 1rem 1.5rem; border-bottom: 1px solid #cad3cb; }
  a { color: inherit; text-underline-offset: .3em; }
  [aria-current] { font-weight: 700; }
  :global(.admin-page h1) { font-size: clamp(2rem, 6vw, 3rem); letter-spacing: -.035em; overflow-wrap: anywhere; hyphens: auto; }
  :global(.admin-page fieldset) { border: 1px solid #cad3cb; padding: 1rem; min-width: 0; }
  :global(.admin-page .check) { display: flex; align-items: baseline; gap: .75rem; }
  :global(.admin-page .actions) { display: flex; gap: .75rem; flex-wrap: wrap; }
  :global(.admin-page input), :global(.admin-page select) { min-width: 0; max-width: 100%; box-sizing: border-box; }
  :global(.admin-page :focus-visible) { outline: 3px solid #176bc1; outline-offset: 3px; }
  :global(.admin-page button:disabled) { opacity: .6; cursor: wait; }
</style>
