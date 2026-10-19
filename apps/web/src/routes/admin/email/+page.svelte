<script lang="ts">
  import { ApiError, createClient } from '@tikkit/api-client'
  import { errorText, fieldText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  // svelte-ignore state_referenced_locally
  const initial = data.smtp.connection
  let connection = $state(initial)
  let host = $state(initial?.host ?? '')
  let port = $state(initial?.port ?? 587)
  let security = $state<'starttls' | 'implicit_tls'>(initial?.security ?? 'starttls')
  let senderName = $state(initial?.sender_name ?? '')
  let senderAddress = $state(initial?.sender_address ?? '')
  let username = $state(initial?.username ?? '')
  let enabled = $state(initial?.enabled ?? true)
  let password = $state('')
  // svelte-ignore state_referenced_locally
  let testTo = $state(data.accountEmail)
  let notice = $state<string | null>(null)
  let error = $state<string | null>(null)
  let fields = $state<Record<string, string[]>>({})

  const api = () => createClient({ language: data.locale })
  const problem = (code: string | null | undefined) =>
    code && code in t.errors ? t.errors[code as keyof typeof t.errors] : null

  function apply(next: typeof connection) {
    connection = next
    if (!next) return
    host = next.host
    port = next.port
    security = next.security
    senderName = next.sender_name
    senderAddress = next.sender_address
    username = next.username
    enabled = next.enabled
    password = ''
  }

  async function save(event: SubmitEvent) {
    event.preventDefault()
    error = null
    notice = null
    fields = {}
    try {
      const saved = await api().request('put', '/api/settings/smtp', {
        body: {
          host,
          port: Number(port),
          security,
          sender_name: senderName,
          sender_address: senderAddress,
          username,
          enabled,
          ...(password ? { password } : {}),
        },
      })
      apply(saved.connection)
      notice = t.saved
    } catch (e) {
      error = errorText(t, e)
      if (e instanceof ApiError) fields = e.fields
    }
  }

  async function remove() {
    error = null
    notice = null
    fields = {}
    try {
      await api().request('delete', '/api/settings/smtp')
      connection = null
      password = ''
      notice = t.smtpRemoved
    } catch (e) {
      error = errorText(t, e)
    }
  }

  async function sendTest(event: SubmitEvent) {
    event.preventDefault()
    error = null
    notice = null
    fields = {}
    try {
      await api().request('post', '/api/settings/smtp/test', { body: { to: testTo } })
      connection = { ...connection!, last_test_error: null, last_test_at: new Date().toISOString() }
      notice = t.smtpTestSent
    } catch (e) {
      error = errorText(t, e)
      if (e instanceof ApiError) {
        fields = e.fields
        if (connection && e.code.startsWith('smtp_') && e.code !== 'smtp_not_configured') {
          connection = {
            ...connection,
            last_test_error: e.code,
            last_test_at: new Date().toISOString(),
          }
        }
      }
    }
  }
</script>

<svelte:head><title>{t.emailSettingsTitle}</title></svelte:head>

<main class="wide">
  <h1>{t.emailSettingsTitle}</h1>
  <p>{t.emailSettingsIntro}</p>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}

  {#if !connection}<p>{t.smtpNotConfigured}</p>{/if}

  <form onsubmit={save} novalidate>
    <label>
      {t.smtpHost}
      <input name="host" bind:value={host} autocomplete="off" aria-invalid={!!fields.host} />
      {#if fields.host}<span class="error">{fieldText(t, fields.host[0])}</span>{/if}
    </label>
    <label>
      {t.smtpPort}
      <input name="port" type="number" min="1" max="65535" bind:value={port} aria-invalid={!!fields.port} />
      {#if fields.port}<span class="error">{fieldText(t, fields.port[0])}</span>{/if}
    </label>
    <label>
      {t.smtpSecurity}
      <select name="security" bind:value={security}>
        <option value="starttls">{t.smtpStarttls}</option>
        <option value="implicit_tls">{t.smtpImplicitTls}</option>
      </select>
      <span class="hint">{t.smtpSecurityHint}</span>
    </label>
    <label>
      {t.smtpSenderName}
      <input name="sender_name" bind:value={senderName} aria-invalid={!!fields.sender_name} />
      {#if fields.sender_name}<span class="error">{fieldText(t, fields.sender_name[0])}</span>{/if}
    </label>
    <label>
      {t.smtpSenderAddress}
      <input name="sender_address" type="email" bind:value={senderAddress} aria-invalid={!!fields.sender_address} />
      {#if fields.sender_address}<span class="error">{fieldText(t, fields.sender_address[0])}</span>{/if}
    </label>
    <label>
      {t.smtpUsername}
      <input name="username" bind:value={username} autocomplete="off" aria-invalid={!!fields.username} />
      {#if fields.username}<span class="error">{fieldText(t, fields.username[0])}</span>{/if}
    </label>
    <label>
      {t.smtpPassword}
      <input name="password" type="password" bind:value={password} autocomplete="new-password" aria-invalid={!!fields.password} />
      {#if connection}<span class="hint">{t.smtpPasswordKeep}</span>{/if}
      {#if fields.password}<span class="error">{fieldText(t, fields.password[0])}</span>{/if}
    </label>
    <label>
      <input name="enabled" type="checkbox" bind:checked={enabled} />
      {t.smtpEnabled}
      <span class="hint">{t.smtpEnabledHint}</span>
    </label>
    <button type="submit">{t.save}</button>
  </form>

  {#if connection}
    {#if problem(connection.last_delivery_error)}
      <p class="error">{t.smtpLastError}: {problem(connection.last_delivery_error)}</p>
    {/if}
    {#if connection.last_test_at}
      <p>
        {#if connection.last_test_error}
          {t.smtpLastTestFailed} {problem(connection.last_test_error) ?? ''}
        {:else}
          {t.smtpLastTestOk}
        {/if}
      </p>
    {/if}

    <h2>{t.smtpTest}</h2>
    <form onsubmit={sendTest} novalidate>
      <label>
        {t.smtpTestTo}
        <input name="to" type="email" bind:value={testTo} aria-invalid={!!fields.to} />
        <span class="hint">{t.smtpTestHint}</span>
        {#if fields.to}<span class="error">{fieldText(t, fields.to[0])}</span>{/if}
      </label>
      <button type="submit">{t.smtpTest}</button>
    </form>
    <p><button type="button" onclick={remove}>{t.smtpRemove}</button></p>
  {/if}
</main>
