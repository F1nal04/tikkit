<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { ApiError, type Language } from '@tikkit/api-client'
  import Alert from '#lib/components/Alert.svelte'
  import Field from '#lib/components/Field.svelte'
  import { api as apiClient } from '#lib/api.ts'
  import { errorText, fieldText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  // Initial values only; the inputs own them afterwards.
  // svelte-ignore state_referenced_locally
  let email = $state(data.user?.email ?? '')
  // svelte-ignore state_referenced_locally
  let language = $state<Language>(data.user?.language ?? data.locale)
  let currentPassword = $state('')
  let newPassword = $state('')
  let profile = $state<{ ok?: boolean; error?: unknown; fields?: Record<string, string[]> }>({})
  let password = $state<{ ok?: boolean; error?: unknown; fields?: Record<string, string[]> }>({})

  const api = () => apiClient(data.locale)
  const languages = $derived([
    { value: 'en', label: t.languages.en },
    { value: 'de', label: t.languages.de },
  ])
  const failure = (e: unknown) => ({
    error: e,
    fields: e instanceof ApiError ? e.fields : {},
  })

  async function saveProfile(event: SubmitEvent) {
    event.preventDefault()
    try {
      await api().request('patch', '/api/auth/me', { body: { email, language } })
      // Reloads the layout, which switches the interface to the new language.
      await invalidateAll()
      profile = { ok: true }
    } catch (e) {
      profile = failure(e)
    }
  }

  async function savePassword(event: SubmitEvent) {
    event.preventDefault()
    try {
      await api().request('post', '/api/auth/password', {
        body: { current_password: currentPassword, new_password: newPassword },
      })
      currentPassword = newPassword = ''
      password = { ok: true }
    } catch (e) {
      password = failure(e)
    }
  }
</script>

<svelte:head><title>{t.profileTitle}</title></svelte:head>

<h1>{t.profileTitle}</h1>
<p>{t.username}: <strong>{data.user?.username}</strong> · {t.role}: {data.user && t.roles[data.user.role]}</p>
<form onsubmit={saveProfile} novalidate>
  <Field
    label={t.email}
    name="email"
    type="email"
    autocomplete="email"
    bind:value={email}
    hint={t.emailHint}
    error={fieldText(t, profile.fields?.email?.[0])}
  />
  <Field label={t.language} name="language" bind:value={language} options={languages} />
  {#if profile.ok}<Alert kind="success">{t.saved}</Alert>{/if}
  {#if profile.error}<Alert>{errorText(t, profile.error)}</Alert>{/if}
  <button type="submit">{t.save}</button>
</form>

<h2>{t.changePassword}</h2>
<form onsubmit={savePassword} novalidate>
  <Field
    label={t.currentPassword}
    name="current_password"
    type="password"
    autocomplete="current-password"
    bind:value={currentPassword}
    error={fieldText(t, password.fields?.current_password?.[0])}
  />
  <Field
    label={t.newPassword}
    name="new_password"
    type="password"
    autocomplete="new-password"
    bind:value={newPassword}
    hint={t.passwordHint}
    error={fieldText(t, password.fields?.new_password?.[0])}
  />
  {#if password.ok}<Alert kind="success">{t.passwordChanged}</Alert>{/if}
  {#if password.error}<Alert>{errorText(t, password.error)}</Alert>{/if}
  <button type="submit">{t.changePassword}</button>
</form>
