<script lang="ts">
  import StatusBadge from '#lib/components/StatusBadge.svelte'
  import Table from '#lib/components/Table.svelte'
  import { messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let r = $derived(t.ai.records)
  let stats = $derived(data.stats)
  const number = $derived(new Intl.NumberFormat(data.locale))
  const date = $derived(new Intl.DateTimeFormat(data.locale, { dateStyle: 'medium', timeStyle: 'short', timeZone: 'UTC' }))
  const when = (value: string) => `${date.format(new Date(value))} UTC`
  // Attempt codes first, then the provider and budget codes of the usage report; unknown codes show as they are.
  const codeLabel = (code: string) => {
    const labels: Record<string, string> = { ...t.ai.usage.outcomeLabels, ...r.codes }
    return labels[code] ?? code
  }
  const href = (outcome: string, cursor = '') => {
    const query = new URLSearchParams()
    if (outcome) query.set('outcome', outcome)
    if (cursor) query.set('cursor', cursor)
    const text = query.toString()
    return text ? `/admin/ai/records?${text}` : '/admin/ai/records'
  }
</script>

<svelte:head><title>{r.title}</title></svelte:head>

<h1>{r.title}</h1>
<p>{r.intro}</p>
<p><a href="/admin/ai">{r.back}</a></p>

<h2>{r.statsTitle}</h2>
<p>{r.statsIntro}</p>
<dl data-testid="ai-outcomes">
  {#each ['solved', 'not_solved', 'skipped', 'failed', 'category_exempt', 'cancelled'] as const as key (key)}
    <dt>{r.stats[key]}</dt>
    <dd>{number.format(stats[key])}</dd>
  {/each}
</dl>
<h3>{r.nowTitle}</h3>
<dl data-testid="ai-open">
  {#each ['processing', 'awaiting_decision'] as const as key (key)}
    <dt>{r.stats[key]}</dt>
    <dd>{number.format(stats[key])}</dd>
  {/each}
</dl>
{#if stats.failures.length}
  <h3>{r.failuresTitle}</h3>
  <Table caption={r.failuresTitle} empty="" isEmpty={false}>
    {#snippet head()}
      <tr><th scope="col">{t.ai.usage.reason}</th><th scope="col">{t.ai.usage.count}</th></tr>
    {/snippet}
    {#each stats.failures as failure (failure.outcome)}
      <tr><th scope="row">{codeLabel(failure.outcome)}</th><td>{number.format(failure.count)}</td></tr>
    {/each}
  </Table>
{/if}

<h2>{r.listTitle}</h2>
<p>{r.listIntro}</p>
<form method="get" class="filters" aria-label={r.filter}>
  <label for="outcome">{r.outcome}</label>
  <select id="outcome" name="outcome" value={data.outcome}>
    <option value="">{r.all}</option>
    {#each data.outcomes as outcome (outcome)}
      <option value={outcome}>{r.outcomes[outcome]}</option>
    {/each}
  </select>
  <button type="submit">{r.apply}</button>
</form>
<Table caption={r.listTitle} empty={r.empty} isEmpty={data.records.items.length === 0}>
  {#snippet head()}
    <tr>
      <th scope="col">{r.ticket}</th>
      <th scope="col">{r.requester}</th>
      <th scope="col">{r.outcome}</th>
      <th scope="col">{r.status}</th>
      <th scope="col">{r.reason}</th>
      <th scope="col">{r.model}</th>
      <th scope="col">{r.submitted}</th>
      <th scope="col">{r.decided}</th>
    </tr>
  {/snippet}
  {#each data.records.items as record (record.id)}
    <tr data-outcome={record.outcome}>
      <th scope="row"><a href={`/admin/submissions/${record.ticket_id}`}>#{record.reference} {record.subject}</a></th>
      <td>{record.requester.display_name} ({record.requester.username})</td>
      <td>{r.outcomes[record.outcome]}</td>
      <td><StatusBadge status={record.status} label={t.statuses[record.status]} /></td>
      <td>{record.outcome_code ? codeLabel(record.outcome_code) : ''}</td>
      <td>{t.ai.providers[record.provider as keyof typeof t.ai.providers] ?? record.provider} · {record.model}</td>
      <td><time datetime={record.created_at}>{when(record.created_at)}</time></td>
      <td>{#if record.decided_at}<time datetime={record.decided_at}>{when(record.decided_at)}</time>{/if}</td>
    </tr>
  {/each}
</Table>
{#if data.cursor || data.records.next_cursor}
  <nav aria-label={r.pages} class="pager">
    {#if data.cursor}<a href={href(data.outcome)}>{r.first}</a>{/if}
    {#if data.records.next_cursor}<a rel="next" href={href(data.outcome, data.records.next_cursor)}>{r.next}</a>{/if}
  </nav>
{/if}

<style>
  p { margin: 0; }
  h3 { margin: 1rem 0 0; }
  dl { display: grid; grid-template-columns: max-content 1fr; gap: 0.25rem 1rem; margin: 0; }
  dd { margin: 0; }
  .filters { display: flex; flex-wrap: wrap; align-items: end; gap: 0.5rem; }
  .pager { display: flex; gap: 1rem; }
</style>
