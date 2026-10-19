<script lang="ts">
  import { ApiError } from '@tikkit/api-client'
  import { api as apiClient } from '#lib/api.ts'
  import Alert from '#lib/components/Alert.svelte'
  import Field from '#lib/components/Field.svelte'
  import { errorText, fieldText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  // Initial values only; the inputs own them afterwards, so a rejected save keeps what was typed.
  // svelte-ignore state_referenced_locally
  const initial = data.smtp.connection
  let connection = $state(initial)
  let form = $state({
    host: initial?.host ?? '',
    port: String(initial?.port ?? 587),
    security: initial?.security ?? 'starttls',
    sender_name: initial?.sender_name ?? '',
    sender_address: initial?.sender_address ?? '',
    username: initial?.username ?? '',
    password: '',
  })
  let enabled = $state(initial?.enabled ?? true)
  // svelte-ignore state_referenced_locally
  let testTo = $state(data.accountEmail)
  let busy = $state(false)
  let notice = $state<string | null>(null)
  let error = $state<unknown>(null)
  let fields = $state<Record<string, string[]>>({})

  let request = $derived(apiClient(data.locale).request)
  const problem = (code: string | null | undefined) =>
    code && code in t.errors ? t.errors[code as keyof typeof t.errors] : null

  async function run(action: () => Promise<string>) {
    if (busy) return
    busy = true
    error = null
    notice = null
    fields = {}
    try {
      notice = await action()
    } catch (e) {
      error = e
      if (e instanceof ApiError) fields = e.fields
    } finally {
      busy = false
    }
  }

  function save(event: SubmitEvent) {
    event.preventDefault()
    return run(async () => {
      const { password, port, ...rest } = form
      const saved = await request('put', '/api/settings/smtp', {
        body: { ...rest, port: Number(port), enabled, ...(password ? { password } : {}) },
      })
      connection = saved.connection
      form.password = ''
      return t.saved
    })
  }

  const remove = () =>
    run(async () => {
      await request('delete', '/api/settings/smtp')
      connection = null
      return t.smtpRemoved
    })

  function sendTest(event: SubmitEvent) {
    event.preventDefault()
    return run(async () => {
      try {
        await request('post', '/api/settings/smtp/test', { body: { to: testTo } })
        connection = connection && { ...connection, last_test_error: null, last_test_at: new Date().toISOString() }
        return t.smtpTestSent
      } catch (e) {
        if (connection && e instanceof ApiError && e.code.startsWith('smtp_') && e.code !== 'smtp_not_configured')
          connection = { ...connection, last_test_error: e.code, last_test_at: new Date().toISOString() }
        throw e
      }
    })
  }
</script>

<svelte:head><title>{t.emailSettingsTitle}</title></svelte:head>

<h1>{t.emailSettingsTitle}</h1>
<p>{t.emailSettingsIntro}</p>
{#if error}<Alert>{errorText(t, error)}</Alert>{/if}
{#if notice}<Alert kind="success">{notice}</Alert>{/if}
{#if !connection}<Alert kind="info">{t.smtpNotConfigured}</Alert>{/if}

<form onsubmit={save} novalidate aria-busy={busy}>
  <Field name="host" label={t.smtpHost} bind:value={form.host} autocomplete="off" error={fieldText(t, fields.host?.[0])} />
  <Field name="port" type="number" label={t.smtpPort} bind:value={form.port} error={fieldText(t, fields.port?.[0])} />
  <Field
    name="security"
    label={t.smtpSecurity}
    bind:value={form.security}
    hint={t.smtpSecurityHint}
    options={[{ value: 'starttls', label: t.smtpStarttls }, { value: 'implicit_tls', label: t.smtpImplicitTls }]}
  />
  <Field name="sender_name" label={t.smtpSenderName} bind:value={form.sender_name} error={fieldText(t, fields.sender_name?.[0])} />
  <Field
    name="sender_address"
    type="email"
    label={t.smtpSenderAddress}
    bind:value={form.sender_address}
    error={fieldText(t, fields.sender_address?.[0])}
  />
  <Field name="username" label={t.smtpUsername} bind:value={form.username} autocomplete="off" error={fieldText(t, fields.username?.[0])} />
  <Field
    name="password"
    type="password"
    label={t.smtpPassword}
    bind:value={form.password}
    autocomplete="new-password"
    hint={connection ? t.smtpPasswordKeep : undefined}
    error={fieldText(t, fields.password?.[0])}
  />
  <label class="check"><input name="enabled" type="checkbox" bind:checked={enabled} />{t.smtpEnabled}</label>
  <p class="hint">{t.smtpEnabledHint}</p>
  <div class="actions"><button type="submit" disabled={busy}>{busy ? t.saving : t.save}</button></div>
</form>

{#if connection}
  {#if problem(connection.last_delivery_error)}
    <Alert>{t.smtpLastError}: {problem(connection.last_delivery_error)}</Alert>
  {/if}
  {#if connection.last_test_at}
    <p>
      {#if connection.last_test_error}{t.smtpLastTestFailed} {problem(connection.last_test_error) ?? ''}{:else}{t.smtpLastTestOk}{/if}
    </p>
  {/if}

  <h2>{t.smtpTest}</h2>
  <form onsubmit={sendTest} novalidate aria-busy={busy}>
    <Field name="to" type="email" label={t.smtpTestTo} bind:value={testTo} hint={t.smtpTestHint} error={fieldText(t, fields.to?.[0])} />
    <div class="actions"><button type="submit" disabled={busy}>{t.smtpTest}</button></div>
  </form>
  <div class="actions"><button type="button" onclick={remove} disabled={busy}>{t.smtpRemove}</button></div>
{/if}

<style>
  form { max-width: 42rem; }
  p { margin: 0; }
</style>
