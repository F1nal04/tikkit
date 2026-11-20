<script lang="ts">
  import Alert from '#lib/components/Alert.svelte'
  import ErrorSummary from '#lib/components/ErrorSummary.svelte'
  import Field from '#lib/components/Field.svelte'
  import { api } from '#lib/api.ts'
  import { focusFormErrors } from '#lib/a11y.ts'
  import { errorText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let username = $state('')
  let sent = $state(false)
  let error = $state<unknown>(null)
  let busy = $state(false)

  async function submit(event: SubmitEvent) {
    event.preventDefault()
    busy = true
    error = null
    try {
      // The answer is the same whether or not the account exists.
      await api(data.locale).request('post', '/api/auth/recovery', { body: { username } })
      sent = true
    } catch (e) {
      error = e
      await focusFormErrors()
    } finally {
      busy = false
    }
  }
</script>

<svelte:head><title>{t.recoveryTitle}</title></svelte:head>

<h1>{t.recoveryTitle}</h1>
{#if sent}
  <Alert kind="success">{t.recoverySent}</Alert>
  <p><a href="/login">{t.signIn}</a></p>
{:else}
  <p>{t.recoveryIntro}</p>
  <form onsubmit={submit} novalidate>
    {#if error}<ErrorSummary title={errorText(t, error)} />{/if}
    <Field label={t.username} name="username" autocomplete="username" required bind:value={username} />
    <button type="submit" disabled={busy}>{t.recoverySubmit}</button>
  </form>
  <p><a href="/login">{t.backToSignIn}</a></p>
{/if}
