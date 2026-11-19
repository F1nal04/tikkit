<script lang="ts">
  import '../styles.css'
  import { afterNavigate, goto } from '$app/navigation'
  import { navigating, page } from '$app/state'
  import Alert from '#lib/components/Alert.svelte'
  import Loading from '#lib/components/Loading.svelte'
  import PublicShell from '#lib/components/PublicShell.svelte'
  import Shell from '#lib/components/Shell.svelte'
  import { api } from '#lib/api.ts'
  import { errorText, messages } from '#lib/i18n.ts'

  let { children, data } = $props()
  let t = $derived(messages[data.locale])
  let logoutError = $state<unknown>(null)
  // SSR sets <html lang>; keep it right when the language changes client-side.
  $effect(() => {
    document.documentElement.lang = data.locale
  })
  // A link that leaves the document would otherwise drop focus on the body. Land on the content.
  afterNavigate((nav) => {
    if (nav.type === 'enter') return
    const active = document.activeElement
    if (!active || active === document.body || !active.isConnected) document.getElementById('main')?.focus()
  })

  async function logout() {
    logoutError = null
    try {
      await api(data.locale).request('post', '/api/auth/logout')
      await goto('/login', { invalidateAll: true })
    } catch (e) {
      logoutError = e
    }
  }
</script>

<a class="skip-link" href="#main">{t.skipToContent}</a>
{#if navigating.to}<Loading label={t.loading} />{/if}
{#if data.user}
  <Shell user={data.user} locale={data.locale} path={page.url.pathname} brand={data.organization?.name} unread={data.unread} onlogout={logout}>
    {#if data.restoreMode}<Alert kind="info">{t.restoreModeBanner}</Alert>{/if}
    {#if logoutError}<Alert>{errorText(t, logoutError)}</Alert>{/if}
    {@render children()}
  </Shell>
{:else}
  <PublicShell locale={data.locale}>{@render children()}</PublicShell>
{/if}
