<script lang="ts">
  import { goto } from '$app/navigation'
  import Alert from '#lib/components/Alert.svelte'
  import Field from '#lib/components/Field.svelte'
  import { api } from '#lib/api.ts'
  import { errorText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let username = $state('')
  let password = $state('')
  let error = $state<string | null>(null)
  let busy = $state(false)

  async function submit(event: SubmitEvent) {
    event.preventDefault()
    busy = true
    error = null
    try {
      await api(data.locale).request('post', '/api/auth/login', { body: { username, password } })
      await goto(data.next ?? '/', { invalidateAll: true })
    } catch (e) {
      error = errorText(t, e)
      password = ''
    } finally {
      busy = false
    }
  }
</script>

<svelte:head><title>{t.loginTitle}</title></svelte:head>

<h1>{t.signIn}</h1>
<form onsubmit={submit} novalidate>
  <Field label={t.username} name="username" autocomplete="username" required bind:value={username} />
  <Field label={t.password} name="password" type="password" autocomplete="current-password" required bind:value={password} />
  {#if error}<Alert>{error}</Alert>{/if}
  <button type="submit" disabled={busy}>{t.signIn}</button>
</form>
<p><a href="/forgot-password">{t.forgotPassword}</a></p>
