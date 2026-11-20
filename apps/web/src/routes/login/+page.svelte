<script lang="ts">
  import { goto } from '$app/navigation'
  import { ApiError } from '@tikkit/api-client'
  import Field from '#lib/components/Field.svelte'
  import ErrorSummary from '#lib/components/ErrorSummary.svelte'
  import { api } from '#lib/api.ts'
  import { fieldItems, focusFormErrors } from '#lib/a11y.ts'
  import { errorText, fieldText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let username = $state('')
  let password = $state('')
  let error = $state<unknown>(null)
  let busy = $state(false)

  async function submit(event: SubmitEvent) {
    event.preventDefault()
    busy = true
    error = null
    try {
      await api(data.locale).request('post', '/api/auth/login', { body: { username, password } })
      await goto(data.next ?? '/', { invalidateAll: true })
    } catch (e) {
      error = e
      if (e instanceof ApiError && e.code === 'invalid_credentials') password = ''
      await focusFormErrors()
    } finally {
      busy = false
    }
  }
</script>

<svelte:head><title>{t.loginTitle}</title></svelte:head>

<h1>{t.signIn}</h1>
<form onsubmit={submit} novalidate>
  {#if error}
    <ErrorSummary
      title={errorText(t, error)}
      items={fieldItems(error instanceof ApiError ? error.fields : {}, { username: t.username, password: t.password }, (code) => fieldText(t, code))}
    />
  {/if}
  <Field label={t.username} name="username" autocomplete="username" required bind:value={username} />
  <Field label={t.password} name="password" type="password" autocomplete="current-password" required bind:value={password} />
  <button type="submit" disabled={busy}>{t.signIn}</button>
</form>
<p><a href="/forgot-password">{t.forgotPassword}</a></p>
