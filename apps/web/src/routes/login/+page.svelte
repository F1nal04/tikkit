<script lang="ts">
  import { goto } from '$app/navigation'
  import { createClient } from '@tikkit/api-client'
  import { errorText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let username = $state('')
  let password = $state('')
  let error = $state<string | null>(null)
  let busy = $state(false)

  async function submit(event: SubmitEvent) {
    event.preventDefault()
    busy = true
    error = null
    try {
      await createClient({ language: data.locale }).request('post', '/api/auth/login', {
        body: { username, password },
      })
      await goto('/', { invalidateAll: true })
    } catch (e) {
      error = errorText(t, e)
      password = ''
    } finally {
      busy = false
    }
  }
</script>

<svelte:head><title>{t.loginTitle}</title></svelte:head>

<main>
  <h1>{t.signIn}</h1>
  <form onsubmit={submit} aria-describedby={error ? 'login-error' : undefined} novalidate>
    <label>
      {t.username}
      <input name="username" autocomplete="username" required bind:value={username} />
    </label>
    <label>
      {t.password}
      <input name="password" type="password" autocomplete="current-password" required bind:value={password} />
    </label>
    {#if error}<p id="login-error" class="error" role="alert">{error}</p>{/if}
    <button type="submit" disabled={busy}>{t.signIn}</button>
  </form>
</main>
