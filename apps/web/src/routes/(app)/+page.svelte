<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import Alert from '#lib/components/Alert.svelte'
  import EmptyState from '#lib/components/EmptyState.svelte'
  import StatusBadge from '#lib/components/StatusBadge.svelte'
  import Table from '#lib/components/Table.svelte'
  import { messages, errorText } from '#lib/i18n.ts'
  import { HISTORY_STATUSES, historyHref, isAiStage } from '#lib/requester.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let h = $derived(t.history)
  let filters = $derived(data.filters)
  let busy = $state(false)
  async function refresh() { busy = true; try { await invalidateAll() } finally { busy = false } }
  const when = (iso: string) => new Date(iso).toLocaleString(data.locale, { timeZone: 'UTC', timeZoneName: 'short' })
</script>

<svelte:head><title>{t.myTicketsTitle}</title></svelte:head>
<h1>{t.myTicketsTitle}</h1>
<p><a class="button" href="/tickets/new">{h.create}</a></p>
<form method="get" class="filters" aria-label={h.filters}>
  <div class="field">
    <label for="history-status">{t.status}</label>
    <select id="history-status" name="status" value={filters.status}>
      <option value="">{h.allStatuses}</option>
      {#each HISTORY_STATUSES as status}<option value={status}>{t.ticketStatuses[status]}</option>{/each}
    </select>
  </div>
  <div class="field"><label for="from">{h.from}</label><input id="from" name="from" type="date" value={filters.from} /></div>
  <div class="field"><label for="until">{h.until}</label><input id="until" name="until" type="date" value={filters.until} min={filters.from || undefined} /></div>
  <div class="actions"><button>{h.apply}</button><a class="button secondary" href="/">{h.clear}</a></div>
</form>
{#if data.unavailable}
  <Alert>{errorText(t, { code: 'unavailable' })}</Alert>
  <button onclick={refresh} disabled={busy}>{h.refresh}</button>
{:else if !data.tickets?.items.length}
  <EmptyState title={h.empty}>{h.emptyHint}</EmptyState>
{:else}
  <Table caption={t.myTicketsTitle} empty={h.empty} isEmpty={false}>
    {#snippet head()}<tr><th scope="col">{h.reference}</th><th scope="col">{h.subject}</th><th scope="col">{t.status}</th><th scope="col">{h.team}</th><th scope="col">{h.assignee}</th><th scope="col">{h.created}</th></tr>{/snippet}
    {#each data.tickets.items as ticket (ticket.id)}
      <tr>
        <th scope="row">#{ticket.reference}</th>
        <td class="wrap"><a href="/tickets/{ticket.id}">{ticket.subject}</a></td>
        <td><StatusBadge status={ticket.status} label={t.ticketStatuses[ticket.status]} /></td>
        <td>{ticket.assignment.team_name ?? (isAiStage(ticket.status) ? h.noHumanTeam : h.generalQueue)}</td><td>{ticket.assignment.assignee_display_name ?? h.unassigned}</td>
        <td><time datetime={ticket.created_at}>{when(ticket.created_at)}</time></td>
      </tr>
    {/each}
  </Table>
{/if}
{#if filters.cursor || data.tickets?.next_cursor}
  <nav aria-label={h.pages} class="pager">
    {#if filters.cursor}<a href={historyHref({ ...filters, cursor: '' })}>{h.first}</a>{/if}
    {#if data.tickets?.next_cursor}<a rel="next" href={historyHref({ ...filters, cursor: data.tickets.next_cursor })}>{h.next}</a>{/if}
  </nav>
{/if}
