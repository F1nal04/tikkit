<script lang="ts">
  import { CSRF_HEADER } from '@tikkit/api-client'
  import Table from '#lib/components/Table.svelte'
  import { messages, errorText } from '#lib/i18n.ts'
  import { reportQuery } from '#lib/reports.ts'
  import { DISPOSITIONS, RESOLUTIONS, submissionsHref, type SubmissionFilters } from '#lib/submissions.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let s = $derived(t.reports)
  let f = $derived(data.filters)
  let r = $derived(data.report)
  let operator = $derived(r.operator)
  let numeric = $derived(new Intl.NumberFormat(data.locale))
  let percent = $derived(new Intl.NumberFormat(data.locale, { style: 'percent', maximumFractionDigits: 1 }))
  let utcDate = $derived(new Intl.DateTimeFormat(data.locale, { dateStyle: 'medium', timeZone: 'UTC' }))
  let hours = $derived(new Intl.NumberFormat(data.locale, { style: 'unit', unit: 'hour', maximumSignificantDigits: 3 }))
  let exporting = $state(false)
  let exportError = $state('')
  let counts = $derived(new Map(r.ai_outcomes.map(({ outcome, count }) => [outcome, count])))
  let closures = $derived(new Map(r.closures.map(({ resolution, count }) => [resolution, count])))
  let timings = $derived([
    [s.firstResponse, operator.first_response_samples, operator.mean_first_response_seconds],
    [s.queueToProposal, operator.time_to_proposal_samples, operator.mean_queue_to_proposal_seconds],
    [s.confirmationWait, operator.confirmation_wait_samples, operator.mean_proposal_to_closure_seconds],
    [s.firstClosure, operator.resolution_samples, operator.mean_queue_to_closure_seconds],
  ] satisfies [string, number, number | null][])
  const drill = (filters: Partial<SubmissionFilters> = {}) =>
    submissionsHref({ ...f, lang: data.locale, ...filters })
  const ratio = (numerator: number | null, denominator: number | null) =>
    numerator !== null && denominator ? `${percent.format(numerator / denominator)} (${numeric.format(numerator)} / ${numeric.format(denominator)})` : s.unavailable

  async function exportCsv() {
    exporting = true
    exportError = ''
    try {
      const query = new URLSearchParams()
      for (const [key, value] of Object.entries(reportQuery(f))) {
        if (value !== undefined) query.set(key, value)
      }
      const response = await fetch(`/api/admin/reports/export?${query}`, { method: 'POST', headers: CSRF_HEADER })
      if (response.status === 413) {
        exportError = s.exportLimit
        return
      }
      if (!response.ok) {
        const body = await response.json()
        exportError = errorText(t, body.error)
        return
      }
      const url = URL.createObjectURL(await response.blob())
      const link = document.createElement('a')
      link.href = url
      link.download = 'ticket-report.csv'
      link.click()
      setTimeout(() => URL.revokeObjectURL(url), 1000)
    } catch {
      exportError = t.errors.generic
    } finally {
      exporting = false
    }
  }
</script>

<svelte:head><title>{s.title}</title></svelte:head>

<h1>{s.title}</h1>

<form method="get" class="filters" aria-label={s.filter}>
  <input type="hidden" name="lang" value={data.locale} />
  <label>{t.submissions.from}<input type="date" name="from" value={f.from} min="0001-01-01" max="9999-12-30" required /></label>
  <label>{t.submissions.until}<input type="date" name="until" value={f.until} min="0001-01-01" max="9999-12-30" required /></label>
  <label>{t.submissions.category}
    <select name="category_id" value={f.category_id}>
      <option value="">{t.submissions.allCategories}</option>
      {#each data.categories as category (category.id)}
        <option value={category.id}>{category.name}{category.archived ? ` (${t.submissions.archived})` : ''}</option>
      {/each}
    </select>
  </label>
  <label>{s.team}
    <select name="team_id" value={f.team_id}>
      <option value="">{s.allTeams}</option>
      {#each data.teams as team (team.id)}
        <option value={team.id}>{team.name}{team.archived ? ` (${t.submissions.archived})` : ''}</option>
      {/each}
    </select>
  </label>
  <button type="submit">{t.submissions.apply}</button>
</form>

<div class="export">
  <button type="button" onclick={exportCsv} disabled={exporting}>
    {exporting ? s.exporting : s.export}
  </button>
  <p>{s.exportRange}</p>
  {#if exportError}<p role="alert">{exportError}</p>{/if}
</div>

{#snippet countHead(label: string)}
  <tr><th scope="col">{label}</th><th scope="col" class="measure">{s.count}</th></tr>
{/snippet}

{#snippet countRow(label: string, count: number, href?: string)}
  <tr>
    <th scope="row">{#if href}<a {href}>{label}</a>{:else}{label}{/if}</th>
    <td class="measure">{numeric.format(count)}</td>
  </tr>
{/snippet}

<section aria-labelledby="volume">
  <h2 id="volume">{s.volume}</h2>
  <p><a href={drill()}>{s.allSubmissions}</a> <strong>{numeric.format(r.submissions)}</strong></p>
  <div class="breakdowns">
    <Table caption={s.day} empty={s.empty} isEmpty={!r.daily.length}>
      {#snippet head()}{@render countHead(s.day)}{/snippet}
      {#each r.daily as row (row.day)}
        {@render countRow(utcDate.format(new Date(`${row.day}T00:00:00Z`)), row.count, drill({ from: row.day, until: row.day }))}
      {/each}
    </Table>
    <Table caption={t.submissions.category} empty={s.empty} isEmpty={!r.categories.length}>
      {#snippet head()}{@render countHead(t.submissions.category)}{/snippet}
      {#each r.categories as row (row.id)}
        {@render countRow(row.name, row.count, drill({ category_id: row.id }))}
      {/each}
    </Table>
    <Table caption={t.submissions.priority} empty={s.empty} isEmpty={!r.priorities.length}>
      {#snippet head()}{@render countHead(t.submissions.priority)}{/snippet}
      {#each r.priorities as row (row.id)}
        {@render countRow(row.name, row.count, drill({ priority_id: row.id }))}
      {/each}
    </Table>
  </div>
</section>

<div class="breakdowns">
  <section aria-labelledby="ai-outcomes">
    <h2 id="ai-outcomes">{s.ai}</h2>
    <dl>
      <dt>{s.attempts}</dt><dd>{numeric.format(r.ai_attempts)}</dd>
      {#if !f.team_id}
        <dt>{s.confirmationRate}</dt><dd>{ratio(r.ai_rate_numerator, r.ai_rate_denominator)}</dd>
        <dt>{s.deflectionRate}</dt><dd>{ratio(r.deflection_rate_numerator, r.deflection_rate_denominator)}</dd>
      {/if}
    </dl>
    {#if f.team_id}<p>{s.teamRatesUnavailable}</p>{/if}
    <Table caption={s.ai} empty={s.empty} isEmpty={false}>
      {#snippet head()}{@render countHead(t.submissions.aiOutcome)}{/snippet}
      {#each DISPOSITIONS as outcome (outcome)}
        {#if outcome !== 'unknown' || counts.has(outcome)}
          {@render countRow(s.aiOutcomes[outcome], counts.get(outcome) ?? 0, counts.has(outcome) ? drill({ ai_disposition: outcome }) : undefined)}
        {/if}
      {/each}
    </Table>
  </section>

  <section aria-labelledby="closures">
    <h2 id="closures">{s.closures}</h2>
    <dl>
      <dt><a href={drill({ status: 'closed' })}>{s.humanClosed}</a></dt>
      <dd>{numeric.format((closures.get('requester_confirmed') ?? 0) + (closures.get('confirmation_timeout') ?? 0))}</dd>
      <dt>{s.initialClosures}</dt><dd>{numeric.format(operator.initial_closures)}</dd>
      <dt class="subcount">{s.initialConfirmed}</dt><dd>{numeric.format(operator.initial_requester_confirmed)}</dd>
      <dt class="subcount">{s.initialTimeout}</dt><dd>{numeric.format(operator.initial_confirmation_timeout)}</dd>
      <dt>{s.subsequentClosures}</dt><dd>{numeric.format(operator.subsequent_closures)}</dd>
      <dt>{s.reopens}</dt><dd>{numeric.format(operator.reopens)}</dd>
      <dt>{s.rejected}</dt><dd>{numeric.format(operator.rejected_proposals)}</dd>
    </dl>
    <Table caption={s.closures} empty={s.empty} isEmpty={false}>
      {#snippet head()}{@render countHead(s.currentOutcome)}{/snippet}
      {#each RESOLUTIONS as resolution (resolution)}
        {@render countRow(t.submissions.resolutions[resolution], closures.get(resolution) ?? 0, closures.has(resolution) ? drill({ resolution }) : undefined)}
      {/each}
      {@render countRow(s.ofWhichAwaiting, r.awaiting_confirmation, r.awaiting_confirmation ? drill({ status: 'awaiting_confirmation' }) : undefined)}
    </Table>
  </section>
</div>

{#snippet workloadCount(count: number, label: string, assignee: string | null, scope?: Partial<SubmissionFilters>)}
  <td class="measure">
    {#if count && scope}
      <a href={drill(scope)} aria-label={`${label} · ${assignee} · ${numeric.format(count)}`}>{numeric.format(count)}</a>
    {:else}
      {numeric.format(count)}
    {/if}
  </td>
{/snippet}

<section aria-labelledby="workload">
  <h2 id="workload">{s.workload}</h2>
  <p>{s.workloadBasis} <a href={drill({ entered_queue: 'true' })}>{s.queued} ({numeric.format(operator.queued)})</a></p>
  <Table caption={s.workload} empty={s.empty} isEmpty={!r.workload.length}>
    {#snippet head()}
      <tr>
        <th scope="col">{s.team}</th><th scope="col">{t.submissions.assignee}</th>
        <th scope="col" class="measure">{s.queued}</th><th scope="col" class="measure">{s.active}</th>
        <th scope="col" class="measure">{s.awaiting}</th>
      </tr>
    {/snippet}
    {#each r.workload as row (`${row.team_id}/${row.assignee_id}`)}
      {@const scope = row.team_id && row.assignee_id
        ? { team_id: row.team_id, assignee_id: row.assignee_id, entered_queue: 'true' } as const
        : undefined}
      <tr>
        <th scope="row">{row.team_name ?? t.submissions.generalQueue}</th>
        <td>
          {row.assignee_name ?? t.submissions.unassigned}
          {#if row.assignee_role}<small>{t.roles[row.assignee_role]}</small>{/if}
        </td>
        {@render workloadCount(row.queued, s.queued, row.assignee_name, scope)}
        {@render workloadCount(row.active, s.active, row.assignee_name, scope && { ...scope, resolution: 'open' })}
        {@render workloadCount(row.awaiting_confirmation, s.awaiting, row.assignee_name, scope && { ...scope, status: 'awaiting_confirmation' })}
      </tr>
    {/each}
  </Table>
</section>

<section aria-labelledby="timing">
  <h2 id="timing">{s.timing}</h2>
  <p>{s.timingBasis}</p>
  <Table caption={s.timing} empty={s.empty} isEmpty={false}>
    {#snippet head()}
      <tr><th scope="col">{s.interval}</th><th scope="col" class="measure">{s.samples}</th><th scope="col" class="measure">{s.mean}</th></tr>
    {/snippet}
    {#each timings as [label, samples, seconds] (label)}
      <tr><th scope="row">{label}</th><td class="measure">{numeric.format(samples)}</td>
        <td class="measure">{seconds === null ? s.unavailable : hours.format(seconds / 3600)}</td></tr>
    {/each}
  </Table>
</section>

<style>
  h2, p, dl { margin: 0; }
  section { display: grid; gap: 0.6rem; align-content: start; min-width: 0; }
  .filters, .export { display: flex; flex-wrap: wrap; align-items: end; gap: 0.75rem 1rem; }
  .export { align-items: center; }
  .filters label { display: grid; gap: 0.25rem; max-width: 100%; min-width: 0; }
  .filters select { min-width: 0; }
  .breakdowns { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 17rem), 1fr)); gap: 1rem; align-items: start; }
  dl { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 0.25rem 0.75rem; }
  dd { margin: 0; text-align: right; font-variant-numeric: tabular-nums; }
  .subcount { padding-inline-start: 1rem; }
  small { display: block; }
  .measure { text-align: right; font-variant-numeric: tabular-nums; }
</style>
