<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { ApiError, type Schemas } from '@tikkit/api-client'
  import { onMount } from 'svelte'
  import { api } from '#lib/api.ts'
  import Alert from '#lib/components/Alert.svelte'
  import Field from '#lib/components/Field.svelte'
  import StatusBadge from '#lib/components/StatusBadge.svelte'
  import { messages, errorText, fieldText } from '#lib/i18n.ts'
  import { attemptFor, type Attempt } from '#lib/intake.ts'
  import ScreenshotGallery from '#lib/ScreenshotGallery.svelte'
  import ScreenshotPicker from '#lib/ScreenshotPicker.svelte'
  import { poll } from '#lib/poll.ts'
  import { publicText } from '#lib/requester.ts'
  import { readOnlyReason, routeOf, type AssignmentInput, type StaffInput } from '#lib/staff.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let s = $derived(t.staff)
  let ticket = $derived(data.ticket)
  let access = $derived(ticket?.staff_access)
  let canHandle = $derived(!!access?.can_handle)
  let reason = $derived(ticket ? readOnlyReason(ticket) : null)
  let route = $derived(ticket ? routeOf(ticket) : null)
  let proposal = $derived(ticket?.transitions.findLast((e) => e.event_type === 'propose_resolution'))
  let priorities = $derived((data.catalog?.priorities ?? []).filter((p) => p.id !== ticket?.priority_id))

  let kind = $state<'public' | 'note'>('public')
  let body = $state('')
  let screenshots = $state<string[]>([])
  let uploading = $state(false)
  let summary = $state('')
  let priorityId = $state('')
  let teamId = $state('')
  let assigneeId = $state('')
  // Set when a transfer took the ticket out of the caller's sight: show the receipt, not the ticket.
  let moved = $state<Schemas['AssignmentReceipt'] | null>(null)
  let busy = $state(false)
  let failure = $state<unknown>(null)
  let fields = $state<Record<string, string[]>>({})
  let feedback = $state<'reply' | 'note' | 'action' | null>(null)
  let changed = $state(false)
  // Someone else changed the ticket while this page had unsent input.
  let elsewhere = $state(false)
  let attempt: Attempt | null = null
  let activeTicketId: string | undefined
  $effect(() => {
    const id = data.ticket?.id
    if (id && id !== activeTicketId) {
      activeTicketId = id
      kind = 'public'; body = ''; screenshots = []; summary = ''; priorityId = ''; teamId = ''; assigneeId = ''; moved = null
      failure = null; fields = {}; feedback = null; changed = false; elsewhere = false; attempt = null
    }
  })
  let more = $state<Schemas['Message'][]>([])
  let moreCursor = $state<string | null | undefined>(undefined)
  let loadingMore = $state(false)
  let messagesPage = $derived([...(data.conversation?.items ?? []), ...more])
  let cursor = $derived(moreCursor === undefined ? data.conversation?.next_cursor : moreCursor)
  // A refresh restarts conversation pagination; it does not discard unsent form input.
  $effect(() => { data.conversation; more = []; moreCursor = undefined })

  const when = (iso: string) => new Date(iso).toLocaleString(data.locale, { timeZone: 'UTC', timeZoneName: 'short' })
  function start() { failure = null; fields = {}; feedback = null; changed = false; elsewhere = false }
  async function refresh() {
    try { await invalidateAll() } catch (e) { failure = e }
  }
  // Stale or revoked: nothing was written. Reload the current state and keep unsent input.
  async function failed(e: unknown) {
    failure = e
    if (e instanceof ApiError) {
      fields = e.fields
      if ([403, 404, 409].includes(e.status)) { changed = true; await refresh() }
    }
  }
  // Pull changes made elsewhere. Every poll is re-authorized: a transfer, team removal, or
  // signed-out session answers 401/404, and the reload replaces the page with that answer.
  onMount(() =>
    poll(async () => {
      if (busy || moved) return
      if (!data.ticket || !data.activity) return refresh()
      let version: string
      try {
        ;({ version } = await api(data.locale).request('get', '/api/tickets/{id}/activity', { path: { id: data.ticket.id } }))
      } catch (e) {
        if (e instanceof ApiError && e.status >= 400 && e.status < 500) return refresh()
        throw e
      }
      if (busy || version === data.activity.version) return
      if (body.trim() || summary.trim() || priorityId || teamId || assigneeId || screenshots.length) elsewhere = true
      await refresh()
    }),
  )
  async function act(input: StaffInput) {
    if (busy || !ticket) return
    start(); busy = true
    try {
      await api(data.locale).request('post', '/api/tickets/{id}/staff-actions', {
        path: { id: ticket.id },
        body: { ...input, expected_updated_at: ticket.updated_at } as Schemas['StaffAction'],
      })
      feedback = 'action'
      if (input.action === 'propose_resolution') summary = ''
      if (input.action === 'set_priority') priorityId = ''
      await refresh()
    } catch (e) { await failed(e) } finally { busy = false }
  }
  async function change(input: AssignmentInput) {
    if (busy || !ticket) return
    start(); busy = true
    try {
      const receipt = await api(data.locale).request('post', '/api/tickets/{id}/assignment', {
        path: { id: ticket.id },
        body: { ...input, expected_updated_at: ticket.updated_at } as Schemas['AssignmentAction'],
      })
      teamId = ''; assigneeId = ''
      if (!receipt.visible) { moved = receipt; return }
      feedback = 'action'
      await refresh()
    } catch (e) { await failed(e) } finally { busy = false }
  }
  async function send() {
    if (busy || uploading || !ticket || !canHandle) return
    start()
    if (!body.trim()) { fields = { body: ['required'] }; return }
    if ([...body].length > 10000) { fields = { body: ['too_long'] }; return }
    busy = true
    const input = { body, screenshot_ids: screenshots }
    attempt = attemptFor(attempt, JSON.stringify({ ticket: ticket.id, kind, ...input }))
    const note = kind === 'note'
    try {
      await api(data.locale).request('post', note ? '/api/tickets/{id}/notes' : '/api/tickets/{id}/replies', {
        path: { id: ticket.id }, body: { ...input, idempotency_key: attempt.key },
      })
      body = ''; screenshots = []; attempt = null; feedback = note ? 'note' : 'reply'
      await refresh()
    } catch (e) { await failed(e) } finally { busy = false }
  }
  async function loadMore() {
    if (loadingMore || !cursor || !ticket) return
    loadingMore = true; failure = null
    try {
      const page = await api(data.locale).request('get', '/api/tickets/{id}/messages', { path: { id: ticket.id }, query: { cursor, limit: 100 } })
      more = [...more, ...page.items]; moreCursor = page.next_cursor
    } catch (e) { failure = e } finally { loadingMore = false }
  }
</script>

<svelte:head><title>{ticket ? `#${ticket.reference} · ${ticket.subject}` : s.unavailable}</title></svelte:head>
<p><a href="/queue">{s.back}</a></p>
{#if data.unavailable || !ticket}
  <h1>{s.unavailable}</h1>
  <Alert>{errorText(t, { code: 'unavailable' })}</Alert>
  <button onclick={refresh}>{s.refresh}</button>
{:else if moved}
  <h1>#{moved.reference}</h1>
  <Alert kind="success">{s.movedAway.replace('{reference}', String(moved.reference)).replace('{team}', moved.team_name ?? '')}</Alert>
  <p><a href="/queue">{s.back}</a></p>
{:else}
  <h1>#{ticket.reference} · {ticket.subject}</h1>
  <div class="actions"><StatusBadge status={ticket.status} label={t.statuses[ticket.status]} /><button class="secondary" onclick={refresh} disabled={busy}>{s.refresh}</button></div>
  {#if feedback}<Alert kind="success">{feedback === 'reply' ? s.replySent : feedback === 'note' ? s.noteSent : s.actionSaved}</Alert>{/if}
  {#if changed}<Alert>{s.changed}</Alert>{:else if elsewhere}<Alert kind="info">{s.updatedElsewhere}</Alert>{:else if failure}<Alert>{errorText(t, failure)}</Alert>{/if}
  {#if reason}<Alert kind="info">{s.readOnly[reason]}</Alert>{/if}

  <dl class="facts">
    <div><dt>{s.requester}</dt><dd>{ticket.requester?.display_name} <span class="muted">({ticket.requester?.username})</span></dd></div>
    <div><dt>{s.team}</dt><dd>{ticket.assignment.team_name ?? s.generalQueue}</dd></div>
    <div><dt>{s.assignee}</dt><dd>{ticket.assignment.assignee_display_name ?? s.unassigned}</dd></div>
    <div><dt>{s.priority}</dt><dd>{ticket.priority_name}</dd></div>
    <div><dt>{s.created}</dt><dd><time datetime={ticket.created_at}>{when(ticket.created_at)}</time></dd></div>
  </dl>

  {#if access && (access.can_claim || access.can_assign || access.can_dispatch)}
    <section aria-labelledby="routing-title">
      <h2 id="routing-title">{s.routing}</h2>
      {#if access.can_claim}
        <p><button disabled={busy} onclick={() => change({ action: 'claim' })}>{s.claim}</button></p>
        <p class="hint">{s.claimHint}</p>
      {/if}
      {#if access.can_assign}
        <form aria-label={s.assign} onsubmit={(e) => { e.preventDefault(); if (assigneeId) change({ action: 'assign', assignee_account_id: assigneeId }) }} novalidate>
          <Field name="assignee_account_id" label={s.assignTo} options={[{ value: '', label: s.chooseAssignee }, ...access.assignees.map((a) => ({ value: a.id, label: a.name }))]} bind:value={assigneeId} error={fieldText(t, fields.assignee_account_id?.[0])} />
          <div class="actions">
            <button disabled={busy || !assigneeId}>{s.assign}</button>
            {#if ticket.assignment.assignee_display_name}<button type="button" class="secondary" disabled={busy} onclick={() => change({ action: 'unassign' })}>{s.unassign}</button>{/if}
          </div>
        </form>
      {/if}
      {#if access.can_dispatch}
        <form aria-label={s.dispatch} onsubmit={(e) => { e.preventDefault(); if (teamId) change({ action: 'dispatch', team_id: teamId }) }} novalidate>
          <Field name="team_id" label={s.dispatchTeam} hint={s.dispatchHint} options={[{ value: '', label: s.chooseTeam }, ...access.dispatch_teams.map((team) => ({ value: team.id, label: team.name }))]} bind:value={teamId} error={fieldText(t, fields.team_id?.[0])} />
          <div class="actions">
            <button disabled={busy || !teamId}>{s.dispatch}</button>
            {#if ticket.assignment.team_name}<button type="button" class="secondary" disabled={busy} onclick={() => change({ action: 'return_to_general' })}>{s.returnToGeneral}</button>{/if}
          </div>
        </form>
      {/if}
    </section>
  {/if}

  <section aria-labelledby="intake-title">
    <h2 id="intake-title">{s.intake}</h2>
    <p class="hint">{s.frozen}</p>
    <dl class="facts">
      <div><dt>{s.category}</dt><dd>{ticket.intake_snapshot?.catalog.category_name ?? ticket.category_name}</dd></div>
      <div><dt>{s.originalPriority}</dt><dd>{ticket.intake_snapshot?.catalog.priority_name ?? ticket.priority_name}</dd></div>
      {#if route}<div><dt>{s.enteredSupport}</dt><dd>{s.routes[route]}</dd></div>{/if}
      {#each ticket.intake_snapshot?.catalog.answers ?? [] as answer (answer.field_id)}<div><dt>{answer.label}</dt><dd class="prose">{answer.value}</dd></div>{/each}
    </dl>
    <p class="prose">{ticket.intake_snapshot?.description ?? ticket.description}</p>
    <ScreenshotGallery ids={ticket.intake_snapshot?.screenshot_ids ?? []} locale={data.locale} />
    {#if route === 'not_solved' || route === 'ai_failed' || ['ai_processing', 'awaiting_ai_decision', 'ai_solved'].includes(ticket.status)}
      <h3>{s.aiSuggestion}</h3>
      <p class="prose">{t.history.noSolution}</p>
    {/if}
  </section>

  {#if ticket.status === 'awaiting_confirmation'}
    <section aria-labelledby="awaiting-title">
      <h2 id="awaiting-title">{s.proposalSummary}</h2>
      <Alert kind="info">{s.awaiting}</Alert>
      <p><strong>{s.proposal} {ticket.proposal_version}</strong></p>
      {#if proposal}<p class="prose">{publicText(proposal.payload, 'summary')}</p>{/if}
      {#if ticket.confirmation_deadline}<p>{s.deadline} <time datetime={ticket.confirmation_deadline}>{when(ticket.confirmation_deadline)}</time></p>{/if}
    </section>
  {/if}

  {#if canHandle}
    <section aria-labelledby="work-title">
      <h2 id="work-title">{s.work}</h2>
      <div class="actions">
        {#if ticket.status === 'queued' || ticket.status === 'waiting_for_requester'}
          <button disabled={busy} onclick={() => act({ action: 'start_work' })}>{ticket.status === 'queued' ? s.startWork : s.resumeWork}</button>
        {/if}
        {#if ticket.status === 'in_progress'}
          <button class="secondary" disabled={busy} onclick={() => act({ action: 'request_information' })}>{s.requestInformation}</button>
        {/if}
      </div>
      {#if ticket.status === 'in_progress'}<p class="hint">{s.requestInformationHint}</p>{/if}

      {#if ticket.status !== 'queued'}
        <form aria-label={s.resolution} onsubmit={(e) => { e.preventDefault(); act({ action: 'propose_resolution', summary }) }} novalidate>
          <Field name="summary" label={s.summary} hint={s.summaryHint} multiline required bind:value={summary} error={fieldText(t, fields.summary?.[0])} />
          <button disabled={busy}>{ticket.status === 'awaiting_confirmation' ? s.replaceProposal : s.propose}</button>
        </form>
      {/if}

      <form aria-label={s.changePriority} onsubmit={(e) => { e.preventDefault(); if (priorityId) act({ action: 'set_priority', priority_id: priorityId }) }} novalidate>
        <Field name="priority_id" label={s.newPriority} options={[{ value: '', label: s.choosePriority }, ...priorities.map((p) => ({ value: p.id, label: p.name }))]} bind:value={priorityId} error={fieldText(t, fields.priority_id?.[0])} />
        <button disabled={busy || !priorityId}>{s.savePriority}</button>
      </form>
    </section>
  {/if}
  {#if !canHandle && summary}
    <section aria-labelledby="unsent-summary-title">
      <h2 id="unsent-summary-title">{s.resolution}</h2>
      <p class="hint">{s.unsentLocked}</p>
      <label for="unsent-summary">{s.summary}</label>
      <textarea id="unsent-summary" readonly value={summary} rows="6"></textarea>
    </section>
  {/if}
  {#if access?.can_reopen}
    <p><button disabled={busy} onclick={() => act({ action: 'reopen' })}>{s.reopen}</button></p>
    <p class="hint">{s.reopenHint}</p>
  {/if}

  <section aria-labelledby="conversation-title">
    <h2 id="conversation-title">{s.conversation}</h2>
    {#if !messagesPage.length}<p>{s.noMessages}</p>{/if}
    <ol class="timeline">
      {#each messagesPage as message (message.id)}
        <li class:note={message.kind === 'internal_note'}>
          <article>
            <h3>{message.author_display_name} · {message.author_kind === 'requester' ? s.requesterAuthor : s.staffAuthor}</h3>
            <p><span class="tag" data-kind={message.kind}>{message.kind === 'internal_note' ? `${s.internalNote} · ${s.staffOnly}` : `${s.publicReply} · ${s.visibleToRequester}`}</span></p>
            <time datetime={message.created_at}>{when(message.created_at)}</time>
            <p class="prose">{message.body}</p>
            <ScreenshotGallery ids={message.screenshot_ids} locale={data.locale} />
          </article>
        </li>
      {/each}
    </ol>
    {#if cursor}<button class="secondary" disabled={loadingMore} onclick={loadMore}>{s.moreMessages}</button>{/if}

    {#if canHandle || body || screenshots.length}
      {#if !canHandle}<p class="hint">{s.unsentLocked}</p>{/if}
      <form class="composer" data-kind={kind} aria-label={s.composer} onsubmit={(e) => { e.preventDefault(); send() }} novalidate>
        <fieldset disabled={busy || !canHandle}>
          <legend>{s.composer}</legend>
          <div role="radiogroup" aria-label={s.sendAs} class="kinds">
            <label><input type="radio" name="kind" value="public" bind:group={kind} /> {s.publicReply}</label>
            <label><input type="radio" name="kind" value="note" bind:group={kind} /> {s.internalNote}</label>
          </div>
          <Alert kind="info">{kind === 'note' ? `${s.staffOnly}. ${s.noteHint}` : `${s.visibleToRequester}. ${s.publicHint}`}</Alert>
          <Field name="body" label={s.message} hint={s.appendOnly} multiline required bind:value={body} error={fieldText(t, fields.body?.[0])} />
          <ScreenshotPicker locale={data.locale} bind:ids={screenshots} bind:uploading disabled={busy} />
          {#if fields.screenshot_ids}<p role="alert">{fieldText(t, fields.screenshot_ids[0])}</p>{/if}
          <button disabled={busy || uploading || !canHandle}>{kind === 'note' ? s.addNote : s.sendReply}</button>
        </fieldset>
      </form>
    {/if}
  </section>

  <section aria-labelledby="history-title">
    <h2 id="history-title">{s.history}</h2>
    <ol class="timeline">
      {#each ticket.transitions as event (event.id)}
        <li>
          <p><strong>{t.history.events[event.event_type as keyof typeof t.history.events] ?? t.statuses[event.to_status]}</strong> · {t.statuses[event.to_status]}</p>
          <time datetime={event.created_at}>{when(event.created_at)}</time>
          <p class="muted">{s.actor}: {event.actor_display_name ?? s.system}</p>
          {#if publicText(event.payload, 'summary')}<p class="prose">{s.summaryLabel}: {publicText(event.payload, 'summary')}</p>{/if}
          {#if publicText(event.payload, 'reason')}<p class="prose">{s.reason}: {publicText(event.payload, 'reason')}</p>{/if}
        </li>
      {/each}
    </ol>
    {#if ticket.priority_changes.length}
      <h3>{s.priorityHistory}</h3>
      <ol class="timeline">
        {#each ticket.priority_changes as change (change.id)}
          <li>
            <p><strong>{s.priorityChanged}</strong>: {change.previous_priority_name} → {change.priority_name}</p>
            <time datetime={change.created_at}>{when(change.created_at)}</time>
            <p class="muted">{s.actor}: {change.actor_display_name ?? s.system}</p>
          </li>
        {/each}
      </ol>
    {/if}
    {#if ticket.assignment_changes.length}
      <h3>{s.assignmentHistory}</h3>
      <ol class="timeline">
        {#each ticket.assignment_changes as entry (entry.id)}
          <li>
            <p><strong>{s.assignmentActions[entry.action as keyof typeof s.assignmentActions] ?? entry.action}</strong>:
              {#if entry.action === 'dispatch' || entry.action === 'return_to_general'}
                {entry.previous_team_name ?? s.generalQueue} → {entry.team_name ?? s.generalQueue}
              {:else}
                {entry.previous_assignee_display_name ?? s.unassigned} → {entry.assignee_display_name ?? s.unassigned}
              {/if}
            </p>
            <time datetime={entry.created_at}>{when(entry.created_at)}</time>
            <p class="muted">{s.actor}: {entry.actor_display_name ?? s.system}</p>
          </li>
        {/each}
      </ol>
    {/if}
  </section>
{/if}
<style>
  h1 { overflow-wrap: anywhere; }
  .facts { display: flex; flex-wrap: wrap; gap: 1rem 2rem; }
  .facts div { min-width: 8rem; max-width: 100%; }
  dt { color: var(--muted); font-size: .875rem; }
  dd { margin: .25rem 0 0; overflow-wrap: anywhere; }
  .muted { color: var(--muted); font-size: .875rem; }
  .prose { white-space: pre-wrap; overflow-wrap: anywhere; }
  .timeline { list-style: none; padding: 0; }
  .timeline > li { padding: .75rem 1rem; margin: .75rem 0; border-left: 3px solid var(--line); background: var(--surface); }
  .timeline > li.note { border-left: 3px dashed var(--muted); }
  .tag { font-size: .875rem; font-weight: 600; }
  h3 { font-size: 1rem; margin: 0 0 .25rem; overflow-wrap: anywhere; }
  time { font-size: .875rem; color: var(--muted); }
  section { margin-top: 1.5rem; }
  fieldset { display: grid; gap: 1rem; }
  .kinds { display: flex; gap: 1.5rem; flex-wrap: wrap; }
  .composer[data-kind='note'] { border-left: 3px dashed var(--muted); padding-left: 1rem; }
</style>
