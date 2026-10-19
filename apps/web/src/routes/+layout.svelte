<script lang="ts">
  import '../styles.css'
  import { goto } from '$app/navigation'
  import { navigating, page } from '$app/state'
  import Loading from '#lib/components/Loading.svelte'
  import PublicShell from '#lib/components/PublicShell.svelte'
  import Shell from '#lib/components/Shell.svelte'
  import { api } from '#lib/api.ts'
  import { messages } from '#lib/i18n.ts'

  let { children, data } = $props()
  let t = $derived(messages[data.locale])
  // SSR sets <html lang>; keep it right when the language changes client-side.
  $effect(() => {
    document.documentElement.lang = data.locale
  })

  async function logout() {
    // Idempotent on the server; leave the page either way.
    await api(data.locale)
      .request('post', '/api/auth/logout')
      .catch(() => undefined)
    await goto('/login', { invalidateAll: true })
  }
</script>

<a class="skip-link" href="#main">{t.skipToContent}</a>
{#if navigating.to}<Loading label={t.loading} />{/if}
{#if data.user}
  <Shell user={data.user} locale={data.locale} path={page.url.pathname} onlogout={logout}>
    {@render children()}
  </Shell>
{:else}
  <PublicShell locale={data.locale}>{@render children()}</PublicShell>
{/if}
