<script lang="ts">
  import { goto } from '$app/navigation'
  import { ApiError, type Language } from '@tikkit/api-client'
  import Alert from '#lib/components/Alert.svelte'
  import Field from '#lib/components/Field.svelte'
  import { api } from '#lib/api.ts'
  import { errorText, fieldText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data, params }: PageProps = $props()
  let t = $derived(messages[data.locale])
  // Initial values only; the inputs own them afterwards.
  // svelte-ignore state_referenced_locally
  let email = $state(data.invitation?.email ?? '')
  // svelte-ignore state_referenced_locally
  let language = $state<Language>(data.invitation?.language ?? data.locale)
  let username = $state('')
  let password = $state('')
  let error = $state<string | null>(null)
  let fields = $state<Record<string, string[]>>({})
  let busy = $state(false)

  async function submit(event: SubmitEvent) {
    event.preventDefault()
    busy = true
    error = null
    fields = {}
    try {
      // The role comes from the invitation on the server, never from this form.
      await api(data.locale).request('post', '/api/invitations/accept', {
        body: { token: params.token, username, email, password, language },
      })
      await goto('/', { invalidateAll: true })
    } catch (e) {
      error = errorText(t, e)
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

<svelte:head><title>{t.inviteTitle}</title></svelte:head>

<h1>{t.inviteTitle}</h1>
{#if !data.invitation}
  <Alert>{errorText(t, { code: data.lookupError })}</Alert>
{:else}
  <p>{t.inviteIntro} <strong>{t.roles[data.invitation.role]}</strong>.</p>
  <form onsubmit={submit} novalidate>
    <Field label={t.username} name="username" autocomplete="username" required bind:value={username} hint={t.usernameHint} error={fieldText(t, fields.username?.[0])} />
    <Field label={t.email} name="email" type="email" autocomplete="email" required bind:value={email} hint={t.emailHint} error={fieldText(t, fields.email?.[0])} />
    <Field label={t.password} name="password" type="password" autocomplete="new-password" required bind:value={password} hint={t.passwordHint} error={fieldText(t, fields.password?.[0])} />
    <Field label={t.language} name="language" bind:value={language} options={languages} />
    {#if error}<Alert>{error}</Alert>{/if}
    <button type="submit" disabled={busy}>{t.inviteSubmit}</button>
  </form>
{/if}
