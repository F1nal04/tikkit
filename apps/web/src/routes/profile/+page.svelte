<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { ApiError, createClient, type Language } from '@tikkit/api-client'
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
  let profile = $state<{ ok?: string; error?: string; fields?: Record<string, string[]> }>({})
  let password = $state<{ ok?: string; error?: string; fields?: Record<string, string[]> }>({})

  const api = () => createClient({ language: data.locale })
  const failure = (e: unknown) => ({
    error: errorText(t, e),
    fields: e instanceof ApiError ? e.fields : {},
  })

  async function saveProfile(event: SubmitEvent) {
    event.preventDefault()
    try {
      await api().request('patch', '/api/auth/me', { body: { email, language } })
      // Reloads the layout, which switches the interface to the new language.
      await invalidateAll()
      profile = { ok: messages[language].saved }
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
      password = { ok: t.passwordChanged }
    } catch (e) {
      password = failure(e)
    }
  }
</script>

<svelte:head><title>{t.profileTitle}</title></svelte:head>

<main>
  <h1>{t.profileTitle}</h1>
  <p>{t.username}: <strong>{data.user?.username}</strong> · {t.role}: {data.user && t.roles[data.user.role]}</p>
  <form onsubmit={saveProfile} novalidate>
    <label>
      {t.email}
      <input name="email" type="email" autocomplete="email" bind:value={email} aria-invalid={!!profile.fields?.email} />
      <span class="hint">{t.emailHint}</span>
      {#if profile.fields?.email}<span class="error">{fieldText(t, profile.fields.email[0])}</span>{/if}
    </label>
    <label>
      {t.language}
      <select name="language" bind:value={language}>
        <option value="en">{t.languages.en}</option>
        <option value="de">{t.languages.de}</option>
      </select>
    </label>
    {#if profile.ok}<p role="status">{profile.ok}</p>{/if}
    {#if profile.error}<p class="error" role="alert">{profile.error}</p>{/if}
    <button type="submit">{t.save}</button>
  </form>

  <h2>{t.changePassword}</h2>
  <form onsubmit={savePassword} novalidate>
    <label>
      {t.currentPassword}
      <input name="current_password" type="password" autocomplete="current-password" bind:value={currentPassword} />
      {#if password.fields?.current_password}<span class="error">{fieldText(t, password.fields.current_password[0])}</span>{/if}
    </label>
    <label>
      {t.newPassword}
      <input name="new_password" type="password" autocomplete="new-password" bind:value={newPassword} />
      <span class="hint">{t.passwordHint}</span>
      {#if password.fields?.new_password}<span class="error">{fieldText(t, password.fields.new_password[0])}</span>{/if}
    </label>
    {#if password.ok}<p role="status">{password.ok}</p>{/if}
    {#if password.error}<p class="error" role="alert">{password.error}</p>{/if}
    <button type="submit">{t.changePassword}</button>
  </form>
</main>
