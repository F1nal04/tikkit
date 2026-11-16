<script lang="ts">
  import { ApiError, type components } from '@tikkit/api-client'
  import { api as apiClient } from '#lib/api.ts'
  import Alert from '#lib/components/Alert.svelte'
  import Field from '#lib/components/Field.svelte'
  import { errorText, fieldText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  type Provider = components['schemas']['AiProvider']

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  // Initial values only; the inputs own them afterwards, so a rejected save keeps what was typed.
  // svelte-ignore state_referenced_locally
  const initial = data.ai
  let ai = $state(initial)
  let form = $state({ provider: initial.provider ?? 'openai', model: initial.model ?? '' })
  let enabled = $state(initial.enabled)
  // Write-only: never prefilled, cleared once stored.
  let keys = $state<Record<Provider, string>>({ openai: '', gemini: '', anthropic: '' })
  // Which provider's key form was last submitted, so its field error shows next to that key.
  let keyFor = $state<Provider | null>(null)
  let busy = $state(false)
  let notice = $state<string | null>(null)
  let error = $state<unknown>(null)
  let fields = $state<Record<string, string[]>>({})

  // The connection test and its last result describe the saved selection, not the selectors.
  let unsaved = $derived(form.provider !== ai.provider || form.model !== ai.model)

  // Only models from the API's supported table that accept screenshots are offered.
  let models = $derived((ai.providers.find((p) => p.provider === form.provider)?.models ?? []).filter((m) => m.image_input))
  // Each provider has its own models: keep the selection inside the current list.
  $effect(() => {
    if (!models.some((m) => m.id === form.model)) form.model = models[0]?.id ?? ''
  })

  let request = $derived(apiClient(data.locale).request)
  let date = $derived(new Intl.DateTimeFormat(data.locale, { dateStyle: 'medium', timeStyle: 'short', timeZone: 'UTC' }))
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
      ai = await request('put', '/api/settings/ai', {
        body: { enabled, provider: form.provider as Provider, model: form.model || null },
      })
      enabled = ai.enabled
      return t.saved
    })
  }

  function saveKey(event: SubmitEvent, provider: Provider) {
    event.preventDefault()
    return run(async () => {
      keyFor = provider
      ai = await request('put', '/api/settings/ai/credentials/{provider}', { path: { provider }, body: { api_key: keys[provider] } })
      keys[provider] = ''
      enabled = ai.enabled
      return t.ai.keySaved
    })
  }

  const removeKey = (provider: Provider) =>
    run(async () => {
      ai = await request('delete', '/api/settings/ai/credentials/{provider}', { path: { provider } })
      enabled = ai.enabled
      return t.ai.keyRemoved
    })

  const test = () =>
    run(async () => {
      try {
        await request('post', '/api/settings/ai/test')
        return t.ai.testOk
      } finally {
        // The stored result is the source of truth, also after a failure.
        ai = await request('get', '/api/settings/ai').catch(() => ai)
      }
    })
</script>

<svelte:head><title>{t.ai.title}</title></svelte:head>

<h1>{t.ai.title}</h1>
<p>{t.ai.intro}</p>
{#if error}<Alert>{errorText(t, error)}</Alert>{/if}
{#if notice}<Alert kind="success">{notice}</Alert>{/if}
<Alert kind="info">{t.ai.status[ai.status]}{#if ai.status === 'ready' && !ai.active} {t.ai.notActive}{/if}</Alert>

<form onsubmit={save} novalidate aria-busy={busy}>
  <Field
    name="provider"
    label={t.ai.provider}
    bind:value={form.provider}
    hint={t.ai.providerHint}
    options={ai.providers.map((p) => ({ value: p.provider, label: t.ai.providers[p.provider] }))}
    error={fieldText(t, fields.provider?.[0])}
  />
  <Field
    name="model"
    label={t.ai.model}
    bind:value={form.model}
    hint={t.ai.modelHint}
    options={models.map((m) => ({ value: m.id, label: m.id }))}
    error={fieldText(t, fields.model?.[0])}
  />
  <label class="check">
    <input name="enabled" type="checkbox" bind:checked={enabled} aria-invalid={!!fields.enabled} aria-describedby="enabled-hint" />{t.ai.enabled}
  </label>
  <p class="hint" id="enabled-hint">
    {#if fields.enabled}<span class="error">{fieldText(t, fields.enabled[0])}</span>{:else}{t.ai.enabledHint}{/if}
  </p>
  <div class="actions"><button type="submit" disabled={busy}>{busy ? t.saving : t.save}</button></div>
</form>

<h2>{t.ai.keys}</h2>
<p>{t.ai.keysIntro}</p>
{#each ai.providers as { provider, credential } (provider)}
  <form onsubmit={(event) => saveKey(event, provider)} novalidate aria-busy={busy} data-provider={provider}>
    <h3>{t.ai.providers[provider]}</h3>
    <p>
      {#if credential}
        {credential.hint ? `${t.ai.keyStored} …${credential.hint}` : t.ai.keyStoredNoHint}
        ({date.format(new Date(credential.set_at))} UTC{credential.set_by_username ? `, ${credential.set_by_username}` : ''})
      {:else}
        {t.ai.keyMissing}
      {/if}
    </p>
    {#if credential && !credential.readable}<Alert>{t.errors.ai_credential_unreadable}</Alert>{/if}
    <Field
      name="api_key_{provider}"
      type="password"
      label="{t.ai.apiKey} ({t.ai.providers[provider]})"
      bind:value={keys[provider]}
      autocomplete="new-password"
      hint={credential ? t.ai.keyReplaceHint : t.ai.keyHint}
      error={keyFor === provider ? fieldText(t, fields.api_key?.[0]) : undefined}
    />
    <div class="actions">
      <button type="submit" disabled={busy}>{credential ? t.ai.keyReplace : t.ai.keySave}</button>
      {#if credential}<button type="button" onclick={() => removeKey(provider)} disabled={busy}>{t.ai.keyRemove}</button>{/if}
    </div>
  </form>
{/each}

<h2>{t.ai.test}</h2>
<p>{t.ai.testIntro}</p>
<p>
  {#if ai.connection.status === 'ok'}{t.ai.lastTestOk}
  {:else if ai.connection.status === 'failed'}{t.ai.lastTestFailed} {problem(ai.connection.error) ?? ''}
  {:else}{t.ai.untested}{/if}
  {#if ai.connection.tested_at}({date.format(new Date(ai.connection.tested_at))} UTC){/if}
</p>
{#if unsaved}<p>{t.ai.testUnsaved}</p>{/if}
<div class="actions"><button type="button" onclick={test} disabled={busy || unsaved}>{t.ai.test}</button></div>

<h2>{t.ai.skip}</h2>
<p>{ai.allow_requester_skip ? t.ai.skipOn : t.ai.skipOff} <a href="/admin/settings">{t.nav.settings}</a></p>

<style>
  form { max-width: 42rem; }
  p { margin: 0; }
  h3 { margin: 1rem 0 0; }
</style>
