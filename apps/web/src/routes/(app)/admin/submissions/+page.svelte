<script lang="ts">
  import StatusBadge from '#lib/components/StatusBadge.svelte'
  import Table from '#lib/components/Table.svelte'
  import { messages } from '#lib/i18n.ts'
  import { AI_OUTCOMES, SOURCES, STATUSES, submissionsHref } from '#lib/submissions.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let s = $derived(t.submissions)
  let f = $derived(data.filters)
  const date = $derived(new Intl.DateTimeFormat(data.locale, { dateStyle: 'medium', timeStyle: 'short', timeZone: 'UTC' }))
  const when = (value: string) => `${date.format(new Date(value))} UTC`
  // Filters without the cursor: the start of the same list.
  let base = $derived({ ...f, cursor: '' })
  let reportFilters = $derived([
    f.team_id && s.team,
    f.assignee_id && s.assignee,
    f.entered_queue && (f.entered_queue === 'true' ? t.reports.queued : s.notHuman),
    f.priority_id && s.priority,
    f.ai_disposition && t.reports.aiOutcomes[f.ai_disposition],
    f.resolution && s.resolutions[f.resolution],
  ].filter(Boolean))
</script>

<svelte:head><title>{s.title}</title></svelte:head>

<h1>{s.title}</h1>
<p>{s.intro}</p>

<form method="get" class="filters" aria-label={s.filter}>
  {#if f.lang}<input type="hidden" name="lang" value={f.lang} />{/if}
  {#if f.team_id}<input type="hidden" name="team_id" value={f.team_id} />{/if}
  {#if f.assignee_id}<input type="hidden" name="assignee_id" value={f.assignee_id} />{/if}
  {#if f.entered_queue}<input type="hidden" name="entered_queue" value={f.entered_queue} />{/if}
  {#if f.priority_id}<input type="hidden" name="priority_id" value={f.priority_id} />{/if}
  {#if f.ai_disposition}<input type="hidden" name="ai_disposition" value={f.ai_disposition} />{/if}
  {#if f.resolution}<input type="hidden" name="resolution" value={f.resolution} />{/if}
  <label>{s.category}
    <select name="category_id" value={f.category_id}>
      <option value="">{s.allCategories}</option>
      {#each data.categories as category (category.id)}
        <option value={category.id}>{category.name}{category.archived ? ` (${s.archived})` : ''}</option>
      {/each}
    </select>
  </label>
  <label>{s.status}
    <select name="status" value={f.status}>
      <option value="">{s.allStatuses}</option>
      {#each STATUSES as status (status)}<option value={status}>{t.statuses[status]}</option>{/each}
    </select>
  </label>
  <label>{s.source}
    <select name="resolution_source" value={f.resolution_source}>
      <option value="">{s.allSources}</option>
      {#each SOURCES as source (source)}<option value={source}>{s.sources[source]}</option>{/each}
    </select>
  </label>
  <label>{s.aiOutcome}
    <select name="ai_outcome" value={f.ai_outcome}>
      <option value="">{s.allOutcomes}</option>
      {#each AI_OUTCOMES as outcome (outcome)}<option value={outcome}>{s.aiOutcomes[outcome]}</option>{/each}
    </select>
  </label>
  <label>{s.from} <input type="date" name="from" value={f.from} /></label>
  <label>{s.until} <input type="date" name="until" value={f.until} /></label>
  <div class="buttons">
    <button type="submit">{s.apply}</button>
    <a href={submissionsHref({ lang: f.lang })}>{s.clear}</a>
  </div>
</form>

{#if reportFilters.length}
  <p>{t.reports.drillDown} {reportFilters.join(' · ')}</p>
{/if}

<Table caption={s.title} empty={s.empty} isEmpty={data.submissions.items.length === 0}>
  {#snippet head()}
    <tr>
      <th scope="col">{s.ticket}</th>
      <th scope="col">{s.status}</th>
      <th scope="col">{s.aiOutcome}</th>
      <th scope="col">{s.resolution}</th>
      <th scope="col">{s.category}</th>
      <th scope="col">{s.requester}</th>
      <th scope="col">{s.submitted}</th>
    </tr>
  {/snippet}
  {#each data.submissions.items as item (item.id)}
    <tr data-ai-outcome={item.ai_outcome ?? 'none'} data-resolution={item.resolution}>
      <th scope="row"><a href={`/admin/submissions/${item.id}`}>#{item.reference} {item.subject}</a></th>
      <td><StatusBadge status={item.status} label={t.statuses[item.status]} /></td>
      <td class="wrap">{item.ai_outcome ? s.aiOutcomes[item.ai_outcome] : s.noAi}</td>
      <td class="wrap">{s.resolutions[item.resolution]}</td>
      <td>{item.category_name}</td>
      <td>{item.requester.display_name} ({item.requester.username})</td>
      <td><time datetime={item.created_at}>{when(item.created_at)}</time></td>
    </tr>
  {/each}
</Table>
{#if f.cursor || data.submissions.next_cursor}
  <nav aria-label={s.pages} class="pager">
    {#if f.cursor}<a href={submissionsHref(base)}>{s.first}</a>{/if}
    {#if data.submissions.next_cursor}<a rel="next" href={submissionsHref({ ...base, cursor: data.submissions.next_cursor })}>{s.next}</a>{/if}
  </nav>
{/if}

<style>
  p { margin: 0; }
  .filters { display: flex; flex-wrap: wrap; align-items: end; gap: 0.75rem 1rem; }
  .filters label { display: grid; grid-template-columns: minmax(0, 1fr); gap: 0.25rem; min-width: 0; max-width: 100%; }
  .filters select { min-width: 0; max-width: 100%; }
  .buttons { display: flex; align-items: center; gap: 1rem; }
  .pager { display: flex; gap: 1rem; }
</style>
