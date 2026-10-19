<script lang="ts">
  import { ApiError, createClient } from '@tikkit/api-client'
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
      await createClient({ language: data.locale }).request('post', '/api/auth/recovery/reset', {
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

<main>
  <h1>{t.resetTitle}</h1>
  {#if done}
    <p role="status">{t.resetDone}</p>
    <a href="/login">{t.signIn}</a>
  {:else}
    <form onsubmit={submit} novalidate>
      <label>
        {t.newPassword}
        <input
          name="new_password"
          type="password"
          autocomplete="new-password"
          required
          bind:value={password}
          aria-invalid={!!fieldError}
          aria-describedby={fieldError ? 'password-hint password-error' : 'password-hint'}
        />
        <span id="password-hint" class="hint">{t.passwordHint}</span>
        {#if fieldError}<span id="password-error" class="error">{fieldError}</span>{/if}
      </label>
      {#if error}
        <p class="error" role="alert">
          {error}
          {#if expired}<a href="/forgot-password">{t.requestNewLink}</a>{/if}
        </p>
      {/if}
      <button type="submit" disabled={busy}>{t.resetSubmit}</button>
    </form>
  {/if}
</main>
