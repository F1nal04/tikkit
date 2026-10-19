<script lang="ts">
  import '../styles.css'
  import { goto } from '$app/navigation'
  import { createClient } from '@tikkit/api-client'
  import { messages } from '#lib/i18n.ts'

  let { children, data } = $props()
  let t = $derived(messages[data.locale])
  // SSR sets <html lang>; keep it right when the language changes client-side.
  $effect(() => {
    document.documentElement.lang = data.locale
  })

  async function logout() {
    // Idempotent on the server; leave the page either way.
    await createClient()
      .request('post', '/api/auth/logout')
      .catch(() => undefined)
    await goto('/login', { invalidateAll: true })
  }
</script>

<header class="topbar">
  <a href="/" class="brand">Tikkit</a>
  {#if data.user}
    {#if data.user.role === 'admin'}<a href="/admin/accounts">{t.admin}</a>{/if}
    <a href="/profile">{t.profile}</a>
    <span>{t.signedInAs} <strong>{data.user.username}</strong></span>
    <button type="button" onclick={logout}>{t.signOut}</button>
  {:else}
    <a href="/login">{t.signIn}</a>
  {/if}
</header>

{@render children()}
