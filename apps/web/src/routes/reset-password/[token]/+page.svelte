<script lang="ts">
  import { ApiError } from '@tikkit/api-client'
  import Alert from '#lib/components/Alert.svelte'
  import Field from '#lib/components/Field.svelte'
  import { api } from '#lib/api.ts'
  import { errorText, fieldText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data, params }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let password = $state('')
  let done = $state(false)
  let error = $state<string | null>(null)
  let expired = $state(false)
  let fieldError = $state<string | undefined>()
  let busy = $state(false)

  async function submit(event: SubmitEvent) {
    event.preventDefault()
    busy = true
    error = null
    fieldError = undefined
    try {
      await api(data.locale).request('post', '/api/auth/recovery/reset', {
        body: { token: params.token, new_password: password },
      })
      done = true
    } catch (e) {
      error = errorText(t, e)
      expired = e instanceof ApiError && e.code === 'reset_token_invalid'
      if (e instanceof ApiError) fieldError = fieldText(t, e.fields.new_password?.[0])
    } finally {
      busy = false
    }
  }
</script>

<svelte:head><title>{t.resetTitle}</title></svelte:head>

<h1>{t.resetTitle}</h1>
{#if done}
  <Alert kind="success">{t.resetDone}</Alert>
  <p><a href="/login">{t.signIn}</a></p>
{:else}
  <form onsubmit={submit} novalidate>
    <Field
      label={t.newPassword}
      name="new_password"
      type="password"
      autocomplete="new-password"
      required
      bind:value={password}
      hint={t.passwordHint}
      error={fieldError}
    />
    {#if error}
      <Alert>
        {error}
        {#if expired}<a href="/forgot-password">{t.requestNewLink}</a>{/if}
      </Alert>
    {/if}
    <button type="submit" disabled={busy}>{t.resetSubmit}</button>
  </form>
{/if}
