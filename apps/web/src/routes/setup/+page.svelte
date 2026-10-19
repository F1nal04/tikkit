<script lang="ts">
  import { goto } from '$app/navigation'
  import { ApiError, type Language } from '@tikkit/api-client'
  import Alert from '#lib/components/Alert.svelte'
  import Field from '#lib/components/Field.svelte'
  import { api } from '#lib/api.ts'
  import { errorText, fieldText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let username = $state('')
  let email = $state('')
  let password = $state('')
  // Initial value only; the select owns it afterwards.
  // svelte-ignore state_referenced_locally
  let language = $state<Language>(data.locale)
  let error = $state<unknown>(null)
  let fields = $state<Record<string, string[]>>({})
  let busy = $state(false)

  async function submit(event: SubmitEvent) {
    event.preventDefault()
    busy = true
    error = null
    fields = {}
    try {
      await api(data.locale).request('post', '/api/setup', {
        body: { username, email, password, language },
      })
      await goto('/', { invalidateAll: true })
    } catch (e) {
      error = e
      if (e instanceof ApiError) fields = e.fields
    } finally {
      busy = false
    }
  }

  const languages = $derived([
    { value: 'en', label: t.languages.en },
    { value: 'de', label: t.languages.de },
  ])
</script>

<svelte:head><title>{t.setupTitle}</title></svelte:head>

<h1>{t.setupTitle}</h1>
<p>{t.setupIntro}</p>
<form onsubmit={submit} novalidate>
  <Field label={t.username} name="username" autocomplete="username" required bind:value={username} hint={t.usernameHint} error={fieldText(t, fields.username?.[0])} />
  <Field label={t.email} name="email" type="email" autocomplete="email" required bind:value={email} hint={t.emailHint} error={fieldText(t, fields.email?.[0])} />
  <Field label={t.password} name="password" type="password" autocomplete="new-password" required bind:value={password} hint={t.passwordHint} error={fieldText(t, fields.password?.[0])} />
  <Field label={t.language} name="language" bind:value={language} options={languages} />
  {#if error}
    <Alert>
      {errorText(t, error)}
      {#if error instanceof ApiError && error.code === 'setup_already_completed'}<a href="/login">{t.signIn}</a>{/if}
    </Alert>
  {/if}
  <button type="submit" disabled={busy}>{t.setupSubmit}</button>
</form>
