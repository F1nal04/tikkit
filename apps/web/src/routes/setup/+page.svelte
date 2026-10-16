<script lang="ts">
  import { goto } from '$app/navigation'
  import { ApiError, createClient, type Language } from '@tikkit/api-client'
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
  let error = $state<string | null>(null)
  let fields = $state<Record<string, string[]>>({})
  let busy = $state(false)

  async function submit(event: SubmitEvent) {
    event.preventDefault()
    busy = true
    error = null
    fields = {}
    try {
      await createClient({ language: data.locale }).request('post', '/api/setup', {
        body: { username, email, password, language },
      })
      await goto('/', { invalidateAll: true })
    } catch (e) {
      error = errorText(t, e)
      if (e instanceof ApiError) fields = e.fields
    } finally {
      busy = false
    }
  }

  const describedBy = (name: string) =>
    [`${name}-hint`, fields[name] && `${name}-error`].filter(Boolean).join(' ')
</script>

<svelte:head><title>{t.setupTitle}</title></svelte:head>

<main>
  <h1>{t.setupTitle}</h1>
  <p>{t.setupIntro}</p>
  <form onsubmit={submit} novalidate>
    <label>
      {t.username}
      <input
        name="username"
        type="text"
        autocomplete="username"
        required
        bind:value={username}
        aria-invalid={!!fields.username}
        aria-describedby={describedBy('username')}
      />
      <span id="username-hint" class="hint">{t.usernameHint}</span>
      {#if fields.username}<span id="username-error" class="error">{fieldText(t, fields.username[0])}</span>{/if}
    </label>
    <label>
      {t.email}
      <input
        name="email"
        type="email"
        autocomplete="email"
        required
        bind:value={email}
        aria-invalid={!!fields.email}
        aria-describedby={describedBy('email')}
      />
      <span id="email-hint" class="hint">{t.emailHint}</span>
      {#if fields.email}<span id="email-error" class="error">{fieldText(t, fields.email[0])}</span>{/if}
    </label>
    <label>
      {t.password}
      <input
        name="password"
        type="password"
        autocomplete="new-password"
        required
        bind:value={password}
        aria-invalid={!!fields.password}
        aria-describedby={describedBy('password')}
      />
      <span id="password-hint" class="hint">{t.passwordHint}</span>
      {#if fields.password}<span id="password-error" class="error">{fieldText(t, fields.password[0])}</span>{/if}
    </label>
    <label>
      {t.language}
      <select name="language" bind:value={language}>
        <option value="en">{t.languages.en}</option>
        <option value="de">{t.languages.de}</option>
      </select>
    </label>
    {#if error}
      <p class="error" role="alert">
        {error}
        {#if error === t.errors.setup_already_completed}<a href="/login">{t.signIn}</a>{/if}
      </p>
    {/if}
    <button type="submit" disabled={busy}>{t.setupSubmit}</button>
  </form>
</main>
