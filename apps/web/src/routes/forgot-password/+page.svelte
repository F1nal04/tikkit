<script lang="ts">
  import { createClient } from '@tikkit/api-client'
  import { errorText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let username = $state('')
  let sent = $state(false)
  let error = $state<string | null>(null)
  let busy = $state(false)

  async function submit(event: SubmitEvent) {
    event.preventDefault()
    busy = true
    error = null
    try {
      // The answer is the same whether or not the account exists.
      await createClient({ language: data.locale }).request('post', '/api/auth/recovery', {
        body: { username },
      })
      sent = true
    } catch (e) {
      error = errorText(t, e)
    } finally {
      busy = false
    }
  }
</script>

<svelte:head><title>{t.recoveryTitle}</title></svelte:head>

<main>
  <h1>{t.recoveryTitle}</h1>
  {#if sent}
    <p role="status">{t.recoverySent}</p>
    <a href="/login">{t.signIn}</a>
  {:else}
    <p>{t.recoveryIntro}</p>
    <form onsubmit={submit} novalidate>
      <label>
        {t.username}
        <input name="username" autocomplete="username" required bind:value={username} />
      </label>
      {#if error}<p class="error" role="alert">{error}</p>{/if}
      <button type="submit" disabled={busy}>{t.recoverySubmit}</button>
    </form>
  {/if}
</main>
