<script lang="ts">
  import type { Snippet } from 'svelte'
  import type { Account } from '@tikkit/api-client'
  import { messages, type Locale } from '#lib/i18n.ts'
  import { activeHref, navFor } from '#lib/nav.ts'
  import { session } from '#lib/session.svelte.ts'
  import Alert from './Alert.svelte'
  import LanguageSelect from './LanguageSelect.svelte'

  type Props = {
    user: Account
    locale: Locale
    path: string
    onlogout: () => void
    children: Snippet
  }
  let { user, locale, path, onlogout, children }: Props = $props()
  let t = $derived(messages[locale])
  let groups = $derived(navFor(user.role))
  let current = $derived(activeHref(path))
  // The menu toggle exists only once scripts run, so without JavaScript the navigation simply stays open.
  let mounted = $state(false)
  let open = $state(false)
  $effect(() => {
    mounted = true
  })
  $effect(() => {
    path
    open = false
  })
</script>

<div class="app">
  <header class="topbar">
    <a href="/" class="brand">Tikkit</a>
    {#if mounted}
      <button type="button" class="menu-toggle secondary" aria-expanded={open} aria-controls="app-menu" onclick={() => (open = !open)}>
        {t.menu}
      </button>
    {/if}
  </header>
  <div class="body">
    <div id="app-menu" class="sidebar" class:collapsed={mounted && !open}>
      <nav aria-label={t.mainNavigation}>
        {#each groups as group (group.section)}
          <h2 class="nav-heading" id="nav-{group.section}">{t.navSections[group.section]}</h2>
          <ul aria-labelledby="nav-{group.section}">
            {#each group.items as item (item.href)}
              <li><a href={item.href} aria-current={item.href === current ? 'page' : undefined}>{t.nav[item.key]}</a></li>
            {/each}
          </ul>
        {/each}
      </nav>
      <section class="account" aria-labelledby="account-heading">
        <h2 class="nav-heading" id="account-heading">{t.account}</h2>
        <p>{t.signedInAs} <strong>{user.username}</strong></p>
        <p>{t.role}: <span class="role">{t.roles[user.role]}</span></p>
        <p><a href="/profile" aria-current={path === '/profile' ? 'page' : undefined}>{t.profile}</a></p>
        <LanguageSelect {locale} {user} />
        <button type="button" class="secondary" onclick={onlogout}>{t.signOut}</button>
      </section>
    </div>
    <main id="main" tabindex="-1">
      {#if session.expired}
        <Alert>
          <strong>{t.sessionExpired}</strong>
          {t.sessionExpiredHint}
          <a href="/login?next={encodeURIComponent(path)}" target="_blank" rel="noopener">{t.signInNewTab}</a>
        </Alert>
      {/if}
      {@render children()}
    </main>
  </div>
</div>
