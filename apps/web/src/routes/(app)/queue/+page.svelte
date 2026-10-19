<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { onMount } from 'svelte'
  import Alert from '#lib/components/Alert.svelte'
  import StatusBadge from '#lib/components/StatusBadge.svelte'
  import Table from '#lib/components/Table.svelte'
  import { errorText, messages } from '#lib/i18n.ts'
  import { age, FILTER_STATUSES, queueHref, SORTS, viewsFor } from '#lib/queue.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let q = $derived(t.queue)
  let params = $derived(data.params)
  let role = $derived(data.user!.role)
  let views = $derived(viewsFor(role))
  let viewLabel = (view: (typeof views)[number]) => (view === 'team' && role === 'admin' ? q.teamAdmin : q[view])
  let items = $derived(data.page?.items ?? [])
  let nextCursor = $derived(data.page?.next_cursor ?? null)
  let retrying = $state(false)

  // Relative ages depend on the clock, so they appear after mount; until then the creation time shows.
  let now = $state<number | null>(null)
  onMount(() => {
    now = Date.now()
    const timer = setInterval(() => (now = Date.now()), 60_000)
    return () => clearInterval(timer)
  })
  const when = (iso: string) =>
    new Intl.DateTimeFormat(data.locale, { dateStyle: 'short', timeStyle: 'short', timeZone: 'UTC' }).format(
      new Date(iso),
    ) + ' UTC'
  const ageText = (iso: string, at: number) => {
    const { value, unit } = age(iso, at)
    return new Intl.NumberFormat(data.locale, { style: 'unit', unit, unitDisplay: 'short' }).format(value)
  }
  async function retry() {
    retrying = true
    try {
      await invalidateAll()
    } finally {
      retrying = false
    }
  }
</script>

<svelte:head><title>{viewLabel(params.view)}</title></svelte:head>

<h1>{viewLabel(params.view)}</h1>

<nav aria-label={q.views} class="tabs">
  <ul>
    {#each views as view (view)}
      <li>
        <a
          href={queueHref({ view })}
          aria-current={view === params.view ? 'page' : undefined}
          data-sveltekit-noscroll
        >
          {viewLabel(view)}
          {#if data.counts}<span class="count">{data.counts[view] ?? ''}</span>{/if}
        </a>
      </li>
    {/each}
  </ul>
</nav>

{#if data.unavailable}
  <Alert>{errorText(t, { code: 'unavailable' })}</Alert>
  <p><button type="button" onclick={retry} disabled={retrying}>{q.retry}</button></p>
{:else}
  <form method="get" class="filters" aria-label={q.filters}>
    <input type="hidden" name="view" value={params.view} />
    <div class="field">
      <label for="status">{q.status}</label>
      <select id="status" name="status" value={params.status}>
        <option value="">{q.openWork}</option>
        {#each FILTER_STATUSES as s (s)}<option value={s}>{t.statuses[s]}</option>{/each}
      </select>
    </div>
    <div class="field">
      <label for="category_id">{q.category}</label>
      <select id="category_id" name="category_id" value={params.category_id}>
        <option value="">{q.anyCategory}</option>
        {#each data.catalog!.categories as c (c.id)}<option value={c.id}>{c.name}</option>{/each}
      </select>
    </div>
    <div class="field">
      <label for="priority_id">{q.priority}</label>
      <select id="priority_id" name="priority_id" value={params.priority_id}>
        <option value="">{q.anyPriority}</option>
        {#each data.catalog!.priorities as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
      </select>
    </div>
    <div class="field">
      <label for="sort">{q.sort}</label>
      <select id="sort" name="sort" value={params.sort}>
        {#each SORTS as s (s)}<option value={s}>{q.sorts[s]}</option>{/each}
      </select>
    </div>
    <label class="check">
      <input type="checkbox" name="unassigned" value="true" checked={params.unassigned} />
      {q.unassignedOnly}
    </label>
    <div class="actions">
      <button type="submit">{q.apply}</button>
      <a class="button secondary" href={queueHref({ view: params.view })}>{q.clear}</a>
    </div>
  </form>

  <Table caption="{viewLabel(params.view)}: {q.caption}" empty={q.empty[params.view]} isEmpty={items.length === 0}>
    {#snippet head()}
      <tr>
        <th scope="col">{q.reference}</th>
        <th scope="col">{q.subject}</th>
        <th scope="col">{q.category}</th>
        <th scope="col">{q.requester}</th>
        <th scope="col">{q.priority}</th>
        <th scope="col">{q.age}</th>
        {#if params.view !== 'general'}<th scope="col">{q.teamColumn}</th>{/if}
        <th scope="col">{q.assignee}</th>
        <th scope="col">{t.status}</th>
      </tr>
    {/snippet}
    {#each items as ticket (ticket.id)}
      <tr>
        <th scope="row">#{ticket.reference}</th>
        <td class="wrap"><a href="/queue/{ticket.id}">{ticket.subject}</a></td>
        <td>{ticket.category_name}</td>
        <td>{ticket.requester.display_name}</td>
        <td>{ticket.priority_name}</td>
        <td>
          <time datetime={ticket.created_at} title={when(ticket.created_at)}>
            {now === null ? when(ticket.created_at) : ageText(ticket.created_at, now)}
          </time>
        </td>
        {#if params.view !== 'general'}<td>{ticket.assignment.team_name ?? ''}</td>{/if}
        <td>{ticket.assignment.assignee_display_name ?? q.unassigned}</td>
        <td><StatusBadge status={ticket.status} label={t.statuses[ticket.status]} /></td>
      </tr>
    {/each}
  </Table>

  {#if params.cursor || nextCursor}
    <nav aria-label={q.pages} class="pager">
      {#if params.cursor}<a href={queueHref({ ...params, cursor: '' })}>{q.first}</a>{/if}
      {#if nextCursor}<a href={queueHref({ ...params, cursor: nextCursor })} rel="next">{q.next}</a>{/if}
    </nav>
  {/if}
{/if}
