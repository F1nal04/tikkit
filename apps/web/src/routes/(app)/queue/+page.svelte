<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { ApiError } from '@tikkit/api-client'
  import { onMount } from 'svelte'
  import { api } from '#lib/api.ts'
  import Alert from '#lib/components/Alert.svelte'
  import Field from '#lib/components/Field.svelte'
  import StatusBadge from '#lib/components/StatusBadge.svelte'
  import Table from '#lib/components/Table.svelte'
  import { errorText, fieldText, messages } from '#lib/i18n.ts'
  import { poll } from '#lib/poll.ts'
  import {
    age,
    BULK_ACTIONS,
    type BulkAction,
    bulkChange,
    FILTER_STATUSES,
    needsTarget,
    queueHref,
    savedViewHref,
    SORTS,
    viewFilters,
    viewsFor,
  } from '#lib/queue.ts'
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

  // Saved views: the API reports filters that no longer apply; links leave them out.
  let viewName = $state('')
  let viewError = $state<unknown>(null)
  let viewNameError = $state<string | undefined>()
  let viewSaved = $state(false)
  let saving = $state(false)
  const staleText = (stale: string[]) =>
    q.staleView.replace('{names}', stale.map((f) => q.staleNames[f as keyof typeof q.staleNames]).join(', '))
  async function saveView(e: SubmitEvent) {
    e.preventDefault()
    saving = true
    viewError = null
    viewNameError = undefined
    viewSaved = false
    try {
      await api(data.locale).request('post', '/api/saved-views', { body: { name: viewName, filters: viewFilters(params) } })
      viewName = ''
      viewSaved = true
      await invalidateAll()
    } catch (err) {
      const fields = err instanceof ApiError ? err.fields : {}
      viewNameError = fieldText(t, fields.name?.[0])
      if (!viewNameError) viewError = err
    } finally {
      saving = false
    }
  }
  async function deleteView(id: string) {
    viewError = null
    viewSaved = false
    try {
      await api(data.locale).request('delete', '/api/saved-views/{id}', { path: { id } })
    } catch (err) {
      if (!(err instanceof ApiError && err.code === 'not_found')) viewError = err
    }
    await invalidateAll()
  }

  // Bulk actions: each ticket is authorized and checked on its own; failures are listed per ticket.
  let selected = $state<string[]>([])
  let chosen = $derived(items.filter((ticket) => selected.includes(ticket.id)))
  let allChosen = $derived(items.length > 0 && chosen.length === items.length)
  let actions = $derived(BULK_ACTIONS.filter((a) => (a !== 'claim' || role !== 'admin') && ((a !== 'assign' && a !== 'unassign') || data.options?.assignees.length)))
  let action = $state<BulkAction>('start_work')
  let target = $state('')
  let applying = $state(false)
  let bulkError = $state<unknown>(null)
  let outcome = $state<{ ok: number; total: number; failed: { reference: number; code: string }[] } | null>(null)
  let targets = $derived(
    action === 'dispatch'
      ? (data.options?.teams ?? []).map((team) => ({ id: team.id, name: team.name }))
      : action === 'assign'
        ? (data.options?.assignees ?? []).map((a) => ({ id: a.id, name: `${a.display_name} (${a.team_name})` }))
        : action === 'set_priority'
          ? (data.catalog?.priorities ?? []).map((p) => ({ id: p.id, name: p.name }))
          : [],
  )
  let targetLabel = $derived(action === 'dispatch' ? q.teamColumn : action === 'assign' ? q.assignee : q.priority)
  const toggleAll = () => (selected = allChosen ? [] : items.map((ticket) => ticket.id))
  const toggle = (id: string) => (selected = selected.includes(id) ? selected.filter((s) => s !== id) : [...selected, id])
  async function applyBulk(e: SubmitEvent) {
    e.preventDefault()
    applying = true
    bulkError = null
    outcome = null
    const tickets = chosen
    try {
      const { results } = await api(data.locale).request('post', '/api/tickets/bulk', {
        body: { change: bulkChange(action, target), tickets: tickets.map((ticket) => ({ id: ticket.id, expected_updated_at: ticket.updated_at })) },
      })
      const failed = results.filter((r) => r.error)
      outcome = {
        ok: results.length - failed.length,
        total: results.length,
        failed: failed.map((r) => ({ reference: tickets.find((ticket) => ticket.id === r.id)!.reference, code: r.error! })),
      }
      // Keep failures selected so they can be retried after the reload brings fresh versions.
      selected = failed.map((r) => r.id)
      await invalidateAll()
    } catch (err) {
      bulkError = err
    } finally {
      applying = false
    }
  }
  const failureText = (code: string) => q.bulkErrors[code as keyof typeof q.bulkErrors] ?? q.bulkErrors.generic

  // Relative ages depend on the clock, so they appear after mount; until then the creation time shows.
  let now = $state<number | null>(null)
  onMount(() => {
    now = Date.now()
    const timer = setInterval(() => (now = Date.now()), 60_000)
    return () => clearInterval(timer)
  })
  // Work done elsewhere shows up without a manual reload; the API re-checks visibility on every poll.
  onMount(() =>
    poll(async (current) => {
      if (!current()) return
      if (!data.counts) return invalidateAll()
      try {
        const counts = await api(data.locale).request('get', '/api/queue/counts')
        if (!current()) return
        if (counts.version !== data.counts.version) await invalidateAll()
      } catch (e) {
        // Signed out or no longer staff: reload into the real answer.
        if (!current()) return
        if (e instanceof ApiError && e.status >= 400 && e.status < 500) await invalidateAll()
        else throw e
      }
    }),
  )
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
    <Field
      label={q.search}
      name="q"
      type="search"
      value={params.q}
      hint={q.searchHint}
      error={data.searchError ? fieldText(t, data.searchError) : undefined}
    />
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

  <section aria-labelledby="saved-views">
    <h2 id="saved-views">{q.savedViews}</h2>
    {#if viewError}<Alert>{errorText(t, viewError)}</Alert>{/if}
    {#if data.views?.length}
      <ul class="saved-views">
        {#each data.views as view (view.id)}
          <li>
            <a href={savedViewHref(view)}>{view.name}</a>
            <button type="button" class="secondary" onclick={() => deleteView(view.id)}>
              <span aria-hidden="true">×</span><span class="sr-only">{q.deleteView.replace('{name}', view.name)}</span>
            </button>
            {#if view.stale.length}<p class="hint">{staleText(view.stale)}</p>{/if}
          </li>
        {/each}
      </ul>
    {/if}
    <form class="filters" onsubmit={saveView} aria-label={q.saveView}>
      <Field label={q.viewName} name="view_name" bind:value={viewName} error={viewNameError} required />
      <div class="actions"><button type="submit" disabled={saving}>{q.save}</button></div>
    </form>
    {#if viewSaved}<Alert kind="success">{q.viewSaved}</Alert>{/if}
  </section>

  {#if chosen.length}
    <form class="filters bulk" onsubmit={applyBulk} aria-label={q.bulk}>
      <p class="count-selected">{q.selected.replace('{count}', String(chosen.length))}</p>
      <div class="field">
        <label for="bulk-action">{q.bulkAction}</label>
        <select id="bulk-action" bind:value={action} onchange={() => (target = '')}>
          {#each actions as a (a)}<option value={a}>{q.bulkActions[a]}</option>{/each}
        </select>
      </div>
      {#if needsTarget(action)}
        <div class="field">
          <label for="bulk-target">{targetLabel}</label>
          <select id="bulk-target" bind:value={target} required>
            <option value="">{q.chooseTarget}</option>
            {#each targets as option (option.id)}<option value={option.id}>{option.name}</option>{/each}
          </select>
        </div>
      {/if}
      <div class="actions"><button type="submit" disabled={applying}>{q.applyBulk}</button></div>
    </form>
  {/if}
  <div aria-live="polite">
    {#if bulkError}<Alert>{errorText(t, bulkError)}</Alert>{/if}
    {#if outcome}
      <Alert kind={outcome.failed.length ? 'info' : 'success'}>
        {q.bulkDone.replace('{ok}', String(outcome.ok)).replace('{total}', String(outcome.total))}
        {#if outcome.failed.length}
          {q.bulkFailed}
          <ul>
            {#each outcome.failed as f (f.reference)}<li>#{f.reference}: {failureText(f.code)}</li>{/each}
          </ul>
        {/if}
      </Alert>
    {/if}
  </div>

  <Table caption="{viewLabel(params.view)}: {q.caption}" empty={q.empty[params.view]} isEmpty={items.length === 0}>
    {#snippet head()}
      <tr>
        <td><input type="checkbox" aria-label={q.selectPage} checked={allChosen} onchange={toggleAll} /></td>
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
        <td>
          <input
            type="checkbox"
            aria-label={q.select.replace('{reference}', String(ticket.reference))}
            checked={selected.includes(ticket.id)}
            onchange={() => toggle(ticket.id)}
          />
        </td>
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
