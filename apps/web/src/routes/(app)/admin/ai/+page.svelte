<script lang="ts">
  import { ApiError, type components } from '@tikkit/api-client'
  import { api as apiClient } from '#lib/api.ts'
  import Alert from '#lib/components/Alert.svelte'
  import Field from '#lib/components/Field.svelte'
  import Table from '#lib/components/Table.svelte'
  import { errorText, fieldText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  type Provider = components['schemas']['AiProvider']
  type Limits = components['schemas']['AiLimitsWrite']
  type LimitField = keyof Limits

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

  // svelte-ignore state_referenced_locally
  let usage = $state(data.usage)
  let days = $state('30')
  // Budgets that may be empty (no limit), then the settings that always need a value.
  const optionalLimits = ['org_request_limit', 'user_request_limit', 'org_token_limit', 'user_token_limit'] as const
  const requiredLimits = [
    'max_concurrency',
    'concurrency_wait_seconds',
    'breaker_failure_threshold',
    'breaker_cooldown_seconds',
    'retention_days',
  ] as const
  const limitFields: LimitField[] = ['window_hours', ...optionalLimits, ...requiredLimits]
  const asText = (limits: typeof data.limits) =>
    Object.fromEntries(limitFields.map((field) => [field, limits[field]?.toString() ?? ''])) as Record<LimitField, string>
  // svelte-ignore state_referenced_locally
  let limits = $state(asText(data.limits))

  // Only models from the API's supported table that accept screenshots are offered.
  let models = $derived((ai.providers.find((p) => p.provider === form.provider)?.models ?? []).filter((m) => m.image_input))
  // Each provider has its own models: keep the selection inside the current list.
  $effect(() => {
    if (!models.some((m) => m.id === form.model)) form.model = models[0]?.id ?? ''
  })

  let request = $derived(apiClient(data.locale).request)
  let date = $derived(new Intl.DateTimeFormat(data.locale, { dateStyle: 'medium', timeStyle: 'short', timeZone: 'UTC' }))
  let number = $derived(new Intl.NumberFormat(data.locale))
  // A token count the provider did not report is shown as such, never as zero.
  const reported = (value: number | null) => (value === null ? t.ai.usage.notReported : number.format(value))
  const limit = (value: number | null) => (value === null ? t.ai.usage.unlimited : number.format(value))
  const outcomeLabel = (code: string) =>
    code in t.ai.usage.outcomeLabels ? t.ai.usage.outcomeLabels[code as keyof typeof t.ai.usage.outcomeLabels] : code
  const problem = (code: string | null | undefined) =>
    code && code in t.errors ? t.errors[code as keyof typeof t.errors] : null

  async function run(action: () => Promise<string | null>) {
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

  const loadUsage = () =>
    run(async () => {
      usage = await request('get', '/api/settings/ai/usage', { query: { days: Number(days) } })
      return null
    })

  function saveLimits(event: SubmitEvent) {
    event.preventDefault()
    return run(async () => {
      // Whole numbers only; an empty budget means no limit. Checked here so the
      // message lands on the field; the API checks the ranges.
      const body: Partial<Record<LimitField, number | null>> = {}
      const invalid: Record<string, string[]> = {}
      for (const field of limitFields) {
        const text = String(limits[field] ?? '').trim()
        const optional = (optionalLimits as readonly string[]).includes(field)
        if (text === '') {
          if (optional) body[field] = null
          else invalid[field] = ['required']
        } else if (/^-?\d{1,15}$/.test(text)) body[field] = Number(text)
        else invalid[field] = ['invalid']
      }
      if (Object.keys(invalid).length) throw new ApiError(422, { error: { code: 'validation_failed', message: '', details: { fields: invalid } }, request_id: '' })
      limits = asText(await request('put', '/api/settings/ai/limits', { body: body as Limits }))
      usage = await request('get', '/api/settings/ai/usage', { query: { days: Number(days) } }).catch(() => usage)
      return t.saved
    })
  }
</script>

<svelte:head><title>{t.ai.title}</title></svelte:head>

<h1>{t.ai.title}</h1>
<p>{t.ai.intro}</p>
{#if error}<Alert>{errorText(t, error)}</Alert>{/if}
{#if notice}<Alert kind="success">{notice}</Alert>{/if}
<Alert kind="info">{t.ai.status[ai.status]}</Alert>

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

<h2>{t.ai.records.title}</h2>
<p>{t.ai.records.intro}</p>
<p><a href="/admin/ai/records">{t.ai.records.link}</a></p>

<h2>{t.ai.usage.title}</h2>
<p>{t.ai.usage.intro}</p>
<dl data-testid="ai-budget-window">
  <dt>{t.ai.usage.window}</dt>
  <dd>{number.format(usage.window.window_hours)} {t.ai.usage.hours}</dd>
  <dt>{t.ai.usage.requests}</dt>
  <dd>{number.format(usage.window.requests)} {t.ai.usage.of} {limit(usage.window.request_limit)}</dd>
  <dt>{t.ai.usage.tokens}</dt>
  <dd>
    {number.format(usage.window.budget_tokens)}
    {t.ai.usage.of}
    {limit(usage.window.token_limit)}
    ({t.ai.usage.estimated}: {number.format(usage.window.estimated_tokens)})
  </dd>
  <dt>{t.ai.usage.inFlight}</dt>
  <dd>{number.format(usage.window.in_flight)} {t.ai.usage.of} {number.format(usage.window.max_concurrency)}</dd>
</dl>
<Field
  name="usage_days"
  label={t.ai.usage.period}
  bind:value={days}
  options={(['7', '30', '90'] as const).map((value) => ({ value, label: t.ai.usage.days[value] }))}
/>
<div class="actions"><button type="button" onclick={loadUsage} disabled={busy}>{t.ai.usage.refresh}</button></div>
{#snippet usageRow(label: string, counts: components['schemas']['AiUsageCounts'], circuit: string)}
  <tr>
    <th scope="row">{label}</th>
    <td>{number.format(counts.requests)}</td>
    <td>{number.format(counts.succeeded)}</td>
    <td>{number.format(counts.failed)}</td>
    <td>{number.format(counts.refused)}</td>
    <td>{number.format(counts.images)}</td>
    <td>{reported(counts.input_tokens)}</td>
    <td>{reported(counts.output_tokens)}</td>
    <td>{reported(counts.total_tokens)}</td>
    <td>{circuit}</td>
  </tr>
{/snippet}
<Table caption={t.ai.usage.caption} empty="" isEmpty={false}>
  {#snippet head()}
    <tr>
      <th scope="col">{t.ai.usage.provider}</th>
      <th scope="col">{t.ai.usage.calls}</th>
      <th scope="col">{t.ai.usage.succeeded}</th>
      <th scope="col">{t.ai.usage.failed}</th>
      <th scope="col">{t.ai.usage.refused}</th>
      <th scope="col">{t.ai.usage.images}</th>
      <th scope="col">{t.ai.usage.inputTokens}</th>
      <th scope="col">{t.ai.usage.outputTokens}</th>
      <th scope="col">{t.ai.usage.totalTokens}</th>
      <th scope="col">{t.ai.usage.circuit}</th>
    </tr>
  {/snippet}
  {#each usage.providers as row (row.provider)}
    {@render usageRow(
      t.ai.providers[row.provider],
      row.usage,
      row.circuit.state === 'open' && row.circuit.open_until
        ? `${t.ai.usage.circuitStates.open} ${date.format(new Date(row.circuit.open_until))} UTC`
        : t.ai.usage.circuitStates[row.circuit.state],
    )}
  {/each}
  {@render usageRow(t.ai.usage.total, usage.totals, '')}
</Table>
<p class="hint">{t.ai.usage.notReportedHint}</p>
{#if usage.totals.usage_unreported > 0}<p>{t.ai.usage.unreported} {number.format(usage.totals.usage_unreported)}</p>{/if}

<h3>{t.ai.usage.outcomes}</h3>
<p>{t.ai.usage.handoff}</p>
<Table caption={t.ai.usage.outcomesCaption} empty={t.ai.usage.noOutcomes} isEmpty={usage.outcomes.length === 0}>
  {#snippet head()}
    <tr><th scope="col">{t.ai.usage.reason}</th><th scope="col">{t.ai.usage.count}</th></tr>
  {/snippet}
  {#each usage.outcomes as outcome (outcome.status + outcome.outcome)}
    <tr><th scope="row">{outcomeLabel(outcome.outcome)}</th><td>{number.format(outcome.count)}</td></tr>
  {/each}
</Table>

<h2>{t.ai.limits.title}</h2>
<p>{t.ai.limits.intro}</p>
<form onsubmit={saveLimits} novalidate aria-busy={busy} data-form="limits">
  <Field
    name="window_hours"
    label={t.ai.limits.window_hours}
    bind:value={limits.window_hours}
    hint={t.ai.limits.window_hoursHint}
    error={fieldText(t, fields.window_hours?.[0])}
  />
  {#each optionalLimits as field (field)}
    <Field
      name={field}
      label={t.ai.limits[field]}
      bind:value={limits[field]}
      hint={field.includes('token') ? t.ai.limits.tokenHint : t.ai.limits.emptyHint}
      error={fieldText(t, fields[field]?.[0])}
    />
  {/each}
  {#each requiredLimits as field (field)}
    <Field
      name={field}
      label={t.ai.limits[field]}
      bind:value={limits[field]}
      hint={t.ai.limits[`${field}Hint`]}
      error={fieldText(t, fields[field]?.[0])}
    />
  {/each}
  <div class="actions"><button type="submit" disabled={busy}>{t.ai.limits.save}</button></div>
</form>

<style>
  form { max-width: 42rem; }
  p { margin: 0; }
  h3 { margin: 1rem 0 0; }
  dl { display: grid; grid-template-columns: max-content 1fr; gap: 0.25rem 1rem; margin: 0; }
  dd { margin: 0; }
</style>
