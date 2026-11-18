<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { ApiError, type Schemas } from '@tikkit/api-client'
  import { api as apiClient } from '#lib/api.ts'
  import Alert from '#lib/components/Alert.svelte'
  import Field from '#lib/components/Field.svelte'
  import Table from '#lib/components/Table.svelte'
  import { errorText, fieldText, messages } from '#lib/i18n.ts'
  import { CONFIRM, COUNTS, PERIODS, exportPersonalData, keepsEverything, periodForm, policyOf, samePolicy, totalOf } from '#lib/retention.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let s = $derived(t.retention)
  let saved = $derived(data.retention.settings)
  let limits = $derived(data.retention.limits)
  const date = $derived(new Intl.DateTimeFormat(data.locale, { dateStyle: 'medium', timeStyle: 'short', timeZone: 'UTC' }))
  const when = (value: string) => `${date.format(new Date(value))} UTC`
  const number = $derived(new Intl.NumberFormat(data.locale))
  const fill = (text: string, values: Record<string, string | number>) => text.replace(/\{(\w+)\}/g, (_, key) => String(values[key] ?? ''))

  // Initial values only; the inputs own them afterwards, so a rejected save keeps what was typed.
  // svelte-ignore state_referenced_locally
  let form = $state(periodForm(data.retention.settings.policy))
  // svelte-ignore state_referenced_locally
  let automatic = $state(data.retention.settings.automatic)
  let saveConfirm = $state('')
  let policy = $derived(policyOf(form))
  let dirty = $derived(!samePolicy(policy, saved.policy) || automatic !== saved.automatic)
  let needsConfirm = $derived(automatic && !keepsEverything(policy))
  let pending = $derived(data.retention.runs.some((run) => run.state === 'pending'))

  let busy = $state('')
  let error = $state<unknown>(null)
  let fields = $state<Record<string, string[]>>({})
  let notice = $state('')
  let preview = $state<Schemas['RetentionPreview'] | null>(null)
  let runConfirm = $state('')

  async function act(name: string, work: () => Promise<void>) {
    if (busy) return
    busy = name
    error = null
    fields = {}
    notice = ''
    try {
      await work()
    } catch (e) {
      error = e
      if (e instanceof ApiError) fields = e.fields
    } finally {
      busy = ''
    }
  }

  const save = (event: SubmitEvent) => {
    event.preventDefault()
    return act('save', async () => {
      await apiClient(data.locale).request('put', '/api/admin/retention', { body: { ...policy, automatic, confirm: needsConfirm ? saveConfirm : null } })
      saveConfirm = ''
      preview = null
      await invalidateAll()
      notice = s.saved
    })
  }
  const runPreview = () =>
    act('preview', async () => {
      preview = null
      preview = await apiClient(data.locale).request('post', '/api/admin/retention/preview', { body: policy })
    })
  const start = (event: SubmitEvent) => {
    event.preventDefault()
    return act('run', async () => {
      await apiClient(data.locale).request('post', '/api/admin/retention/runs', { body: { confirm: runConfirm } })
      runConfirm = ''
      preview = null
      await invalidateAll()
      notice = s.started
    })
  }
  const refresh = () => act('refresh', () => invalidateAll())

  // Personal data of one account.
  let lookup = $state('')
  let account = $state<Schemas['PersonalDataSummary'] | null>(null)
  let typed = $state('')
  let purge = $state(false)
  let anonymized = $state<Schemas['AnonymizationResult'] | null>(null)

  const find = (event: SubmitEvent) => {
    event.preventDefault()
    return act('find', async () => {
      account = null
      anonymized = null
      typed = ''
      purge = false
      account = await apiClient(data.locale).request('get', '/api/admin/personal-data', { query: { username: lookup.trim() } })
    })
  }
  const download = () =>
    act('export', async () => {
      if (!account) return
      const result = await exportPersonalData(account.account_id, typed)
      const url = URL.createObjectURL(result.blob)
      const link = document.createElement('a')
      link.href = url
      link.download = result.filename
      link.click()
      URL.revokeObjectURL(url)
      notice = s.exported
    })
  const anonymize = () =>
    act('anonymize', async () => {
      if (!account) return
      anonymized = await apiClient(data.locale).request('post', '/api/admin/accounts/{id}/anonymize', {
        path: { id: account.account_id },
        body: { username: typed, purge_ticket_content: purge },
      })
      account = null
      typed = ''
      notice = s.anonymized
    })
</script>

<svelte:head><title>{s.title}</title></svelte:head>

<h1>{s.title}</h1>
<p>{s.intro}</p>
{#if error}<Alert>{errorText(t, error)}</Alert>{/if}
{#if notice}<Alert kind="success">{notice}</Alert>{/if}

<section aria-labelledby="policy-heading">
  <h2 id="policy-heading">{s.policy}</h2>
  <p class="hint">{fill(s.policyHint, { min: limits.min_days, max: number.format(limits.max_days), audit: limits.audit_min_days })}</p>
  <form onsubmit={save} novalidate aria-busy={busy === 'save'}>
    {#each PERIODS as key (key)}
      <Field name={key} type="number" label={s.periods[key]} hint={s.periodHints[key]} bind:value={form[key]} error={fieldText(t, fields[key]?.[0])} />
    {/each}
    <label class="check"><input type="checkbox" bind:checked={automatic} />{s.automatic}</label>
    <p class="hint">{s.automaticHint}</p>
    {#if needsConfirm}
      <Field name="save_confirm" label={fill(s.typeToConfirm, { word: CONFIRM })} hint={s.automaticConfirmHint} autocomplete="off" bind:value={saveConfirm} error={fieldText(t, fields.confirm?.[0])} />
    {/if}
    <div class="actions">
      <button type="submit" disabled={!!busy || (needsConfirm && saveConfirm !== CONFIRM)}>{busy === 'save' ? t.saving : s.save}</button>
      <button type="button" class="secondary" onclick={runPreview} disabled={!!busy}>{s.preview}</button>
    </div>
  </form>
</section>

{#if preview}
  <section aria-labelledby="preview-heading" data-testid="preview">
    <h2 id="preview-heading">{s.previewTitle}</h2>
    <p>{totalOf(preview.affected) === 0 ? s.previewNothing : fill(s.previewIntro, { time: when(preview.as_of) })}</p>
    <Table caption={s.previewTitle} empty="" isEmpty={false}>
      {#snippet head()}
        <tr><th scope="col">{s.category}</th><th scope="col">{s.affected}</th></tr>
      {/snippet}
      {#each COUNTS as key (key)}
        <tr data-count={key}><th scope="row">{s.counts[key]}</th><td>{number.format(preview.affected[key])}</td></tr>
      {/each}
    </Table>
    <p class="hint">{s.previewHint}</p>
  </section>
{/if}

<section aria-labelledby="run-heading">
  <h2 id="run-heading">{s.run}</h2>
  {#if dirty}
    <p>{s.runDirty}</p>
  {:else if keepsEverything(saved.policy)}
    <p>{s.runNothing}</p>
  {:else if pending}
    <p>{s.runPending}</p>
    <div class="actions"><button type="button" class="secondary" onclick={refresh} disabled={!!busy}>{s.refresh}</button></div>
  {:else}
    <p>{s.runHint}</p>
    <form onsubmit={start} novalidate aria-busy={busy === 'run'}>
      <Field name="run_confirm" label={fill(s.typeToConfirm, { word: CONFIRM })} autocomplete="off" bind:value={runConfirm} error={fieldText(t, fields.confirm?.[0])} />
      <div class="actions"><button type="submit" disabled={!!busy || runConfirm !== CONFIRM}>{s.runNow}</button></div>
    </form>
  {/if}

  <Table caption={s.runs} empty={s.noRuns} isEmpty={data.retention.runs.length === 0}>
    {#snippet head()}
      <tr>
        <th scope="col">{s.started_at}</th>
        <th scope="col">{s.startedBy}</th>
        <th scope="col">{s.state}</th>
        {#each COUNTS as key (key)}<th scope="col">{s.counts[key]}</th>{/each}
      </tr>
    {/snippet}
    {#each data.retention.runs as run (run.id)}
      <tr data-run={run.state}>
        <td><time datetime={run.created_at}>{when(run.created_at)}</time></td>
        <td>{run.requested_by ? (run.requested_by_username ?? run.requested_by) : s.automaticRun}</td>
        <td>{s.states[run.state]}</td>
        {#each COUNTS as key (key)}<td>{number.format(run.deleted[key])}</td>{/each}
      </tr>
    {/each}
  </Table>
</section>

<section aria-labelledby="person-heading">
  <h2 id="person-heading">{s.person}</h2>
  <p>{s.personIntro}</p>
  <form onsubmit={find} novalidate class="inline" aria-busy={busy === 'find'}>
    <Field name="lookup" label={s.username} autocomplete="off" bind:value={lookup} />
    <div class="actions"><button type="submit" class="secondary" disabled={!!busy || lookup.trim() === ''}>{s.find}</button></div>
  </form>

  {#if account}
    <div data-testid="account">
      <h3>{account.username} <small>({t.roles[account.role]}, {account.anonymized_at ? s.isAnonymized : account.is_active ? s.isActive : s.isInactive})</small></h3>
      <small class="id">{account.account_id}</small>
      <dl>
        <dt>{s.stored.tickets}</dt><dd>{number.format(account.tickets)}</dd>
        <dt>{s.stored.openTickets}</dt><dd>{number.format(account.open_tickets)}</dd>
        <dt>{s.stored.finalTickets}</dt><dd>{number.format(account.final_tickets_with_content)}</dd>
        <dt>{s.stored.drafts}</dt><dd>{number.format(account.drafts)}</dd>
        <dt>{s.stored.pendingScreenshots}</dt><dd>{number.format(account.pending_screenshots)}</dd>
        <dt>{s.stored.sessions}</dt><dd>{number.format(account.sessions)}</dd>
        <dt>{s.stored.auditEvents}</dt><dd>{number.format(account.audit_events)}</dd>
      </dl>
      <Field name="typed_username" label={s.typeUsername} hint={s.typeUsernameHint} autocomplete="off" bind:value={typed} error={fieldText(t, fields.username?.[0])} />
      <div class="actions"><button type="button" onclick={download} disabled={!!busy || typed.trim() === ''}>{s.export}</button></div>
      <p class="hint">{s.exportHint}</p>

      <h3>{s.anonymize}</h3>
      {#if account.anonymized_at}
        <p>{s.alreadyAnonymized}</p>
      {:else if account.is_active}
        <p>{s.deactivateFirst} <a href="/admin/accounts">{t.nav.accounts}</a></p>
      {:else}
        <p>{s.anonymizeHint}</p>
        <label class="check"><input type="checkbox" bind:checked={purge} />{s.purgeContent}</label>
        {#if account.open_tickets > 0}<p class="hint">{s.openStay}</p>{/if}
        {#if fields.account}<Alert>{fieldText(t, fields.account[0])}</Alert>{/if}
        <div class="actions"><button type="button" onclick={anonymize} disabled={!!busy || typed.trim() === ''}>{s.anonymizeNow}</button></div>
      {/if}
    </div>
  {/if}
  {#if anonymized}
    <p data-testid="anonymized">{fill(s.anonymizedAs, { username: anonymized.username })}</p>
  {/if}
</section>

<style>
  p { margin: 0; }
  section { display: grid; gap: 0.75rem; margin-top: 1.5rem; }
  form { display: grid; gap: 0.75rem; max-width: 42rem; }
  .actions { display: flex; flex-wrap: wrap; gap: 0.5rem 1rem; }
  dl { display: grid; grid-template-columns: minmax(0, auto) minmax(0, 1fr); gap: 0.25rem 1rem; margin: 0; max-width: 42rem; }
  dd { margin: 0; }
  .id { display: block; font-size: 0.8em; overflow-wrap: anywhere; }
  h3 { margin: 0.5rem 0 0; }
</style>
