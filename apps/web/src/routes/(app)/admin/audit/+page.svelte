<script lang="ts">
  import { auditHref, exportAudit, hasMetadata, metadataText, type AuditExport } from '#lib/audit.ts'
  import Alert from '#lib/components/Alert.svelte'
  import Table from '#lib/components/Table.svelte'
  import { errorText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let s = $derived(t.audit)
  let f = $derived(data.filters)
  const date = $derived(new Intl.DateTimeFormat(data.locale, { dateStyle: 'medium', timeStyle: 'medium', timeZone: 'UTC' }))
  const when = (value: string) => `${date.format(new Date(value))} UTC`
  // Filters without the cursor: the start of the same list.
  let base = $derived({ ...f, cursor: '' })
  // An action that is filtered on stays selectable even when no event has it (any more).
  let actions = $derived(f.action && !data.actions.includes(f.action) ? [f.action, ...data.actions] : data.actions)
  const services = $derived(s.services as Record<string, string>)

  let busy = $state(false)
  let exported = $state<AuditExport | null>(null)
  let error = $state<unknown>(null)

  async function download() {
    if (busy) return
    busy = true
    exported = null
    error = null
    try {
      const result = await exportAudit(f)
      const url = URL.createObjectURL(result.blob)
      const link = document.createElement('a')
      link.href = url
      link.download = result.filename
      link.click()
      URL.revokeObjectURL(url)
      exported = result
    } catch (e) {
      error = e
    } finally {
      busy = false
    }
  }
</script>

<svelte:head><title>{s.title}</title></svelte:head>

<h1>{s.title}</h1>
<p>{s.intro}</p>

<form method="get" class="filters" aria-label={s.filter}>
  <label>{s.actor} <input name="actor_username" value={f.actor_username} maxlength="64" autocomplete="off" /></label>
  <label>{s.action}
    <select name="action" value={f.action}>
      <option value="">{s.allActions}</option>
      {#each actions as action (action)}<option value={action}>{action}</option>{/each}
    </select>
  </label>
  <label>{s.ticket} <input name="ticket_reference" value={f.ticket_reference} inputmode="numeric" maxlength="16" autocomplete="off" /></label>
  <label>{s.requestId} <input name="request_id" value={f.request_id} maxlength="128" autocomplete="off" /></label>
  <label>{s.from} <input type="date" name="from" value={f.from} /></label>
  <label>{s.until} <input type="date" name="until" value={f.until} /></label>
  {#if f.resource_type}<input type="hidden" name="resource_type" value={f.resource_type} />{/if}
  {#if f.resource_id}<input type="hidden" name="resource_id" value={f.resource_id} />{/if}
  <div class="buttons">
    <button type="submit">{s.apply}</button>
    <a href="/admin/audit">{s.clear}</a>
  </div>
</form>
{#if f.resource_type}
  <p data-testid="resource-filter">{s.resourceFilter} <code>{f.resource_type}{f.resource_id ? ` ${f.resource_id}` : ''}</code></p>
{/if}

<div class="export">
  <button type="button" onclick={download} disabled={busy} aria-busy={busy}>{s.export}</button>
  <span>{s.exportHint}</span>
</div>
{#if error}<Alert>{errorText(t, error)}</Alert>{/if}
{#if exported}
  <Alert kind="success">{(exported.truncated ? s.exportedTruncated : s.exported).replace('{rows}', String(exported.rows))}</Alert>
{/if}

<Table caption={s.title} empty={s.empty} isEmpty={data.events.items.length === 0}>
  {#snippet head()}
    <tr>
      <th scope="col">{s.time}</th>
      <th scope="col">{s.action}</th>
      <th scope="col">{s.actor}</th>
      <th scope="col">{s.resource}</th>
      <th scope="col">{s.requestId}</th>
      <th scope="col">{s.details}</th>
    </tr>
  {/snippet}
  {#each data.events.items as event (event.id)}
    <tr id={`audit-${event.id}`} data-action={event.action}>
      <td><time datetime={event.created_at}>{when(event.created_at)}</time></td>
      <th scope="row"><code>{event.action}</code></th>
      <td class="wrap">
        {#if event.actor.account_id}
          <a href={auditHref({ actor_username: event.actor.username ?? '' })}>{event.actor.username}</a>
          {#if event.actor.role}<span>({t.roles[event.actor.role]}{event.actor.team_name ? `, ${event.actor.team_name}` : ''})</span>{/if}
          <small class="id">{event.actor.account_id}</small>
        {:else}
          <span data-testid="service">{services[event.actor.service ?? ''] ?? event.actor.service}</span>
        {/if}
      </td>
      <td class="wrap">
        {#if event.ticket_id}
          <a href={auditHref({ ticket_reference: String(event.ticket_reference ?? '') })}>{s.ticketNumber.replace('{reference}', String(event.ticket_reference ?? '?'))}</a>
          <a href={`/admin/submissions/${event.ticket_id}`}>{s.openSubmission}</a>
        {:else if event.resource_type}
          <a href={auditHref({ resource_type: event.resource_type, resource_id: event.resource_id ?? '' })}><code>{event.resource_type}</code></a>
          {#if event.subject}<span>{event.subject.username}</span>{/if}
          {#if event.resource_id}<small class="id">{event.resource_id}</small>{/if}
        {:else}
          <span>{s.none}</span>
        {/if}
      </td>
      <td class="wrap">
        {#if event.request_id}<a class="id" href={auditHref({ request_id: event.request_id })} title={s.sameRequest}>{event.request_id}</a>{:else}<span>{s.none}</span>{/if}
      </td>
      <td>
        {#if hasMetadata(event.metadata)}
          <details><summary>{s.show}</summary><pre>{metadataText(event.metadata)}</pre></details>
        {:else}
          <span>{s.none}</span>
        {/if}
      </td>
    </tr>
  {/each}
</Table>
{#if f.cursor || data.events.next_cursor}
  <nav aria-label={s.pages} class="pager">
    {#if f.cursor}<a href={auditHref(base)}>{s.first}</a>{/if}
    {#if data.events.next_cursor}<a rel="next" href={auditHref({ ...base, cursor: data.events.next_cursor })}>{s.next}</a>{/if}
  </nav>
{/if}

<style>
  p { margin: 0; }
  .filters { display: flex; flex-wrap: wrap; align-items: end; gap: 0.75rem 1rem; }
  .filters label { display: grid; grid-template-columns: minmax(0, 1fr); gap: 0.25rem; min-width: 0; max-width: 100%; }
  .filters select, .filters input { min-width: 0; max-width: 100%; }
  .buttons, .export { display: flex; flex-wrap: wrap; align-items: center; gap: 0.5rem 1rem; }
  .pager { display: flex; gap: 1rem; }
  .id { display: block; font-size: 0.8em; overflow-wrap: anywhere; }
  pre { margin: 0.25rem 0 0; max-width: 28rem; white-space: pre-wrap; overflow-wrap: anywhere; font-size: 0.85em; }
</style>
