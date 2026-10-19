<script lang="ts">
  import '../styles.css'
  import { goto } from '$app/navigation'
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
  <Shell user={data.user} locale={data.locale} path={page.url.pathname} onlogout={logout}>
    {#if logoutError}<Alert>{errorText(t, logoutError)}</Alert>{/if}
    {@render children()}
  </Shell>
{:else}
  <PublicShell locale={data.locale}>{@render children()}</PublicShell>
{/if}
