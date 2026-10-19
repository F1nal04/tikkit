<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { ApiError, type Schemas } from '@tikkit/api-client'
  import { api } from '#lib/api.ts'
  import Alert from '#lib/components/Alert.svelte'
  import Field from '#lib/components/Field.svelte'
  import StatusBadge from '#lib/components/StatusBadge.svelte'
  import ScreenshotGallery from '#lib/ScreenshotGallery.svelte'
  import ScreenshotPicker from '#lib/ScreenshotPicker.svelte'
  import { messages, errorText, fieldText } from '#lib/i18n.ts'
  import { attemptFor, type Attempt } from '#lib/intake.ts'
  import { requesterActions, publicText, isAiStage } from '#lib/requester.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let h = $derived(t.history)
  let ticket = $derived(data.ticket)
  let actions = $derived(ticket && data.policy ? requesterActions(ticket, data.policy) : null)
  let proposal = $derived(ticket?.transitions.findLast((e) => e.event_type === 'propose_resolution'))
  let body = $state('')
  let screenshots = $state<string[]>([])
  let uploading = $state(false)
  let reason = $state('')
  let cancelling = $state(false)
  let busy = $state(false)
  let failure = $state<unknown>(null)
  let fields = $state<Record<string, string[]>>({})
  let feedback = $state<'reply' | 'action' | null>(null)
  let changed = $state(false)
  let attempt: Attempt | null = null
  let activeTicketId: string | undefined
  $effect(() => {
    const id = data.ticket?.id
    if (id && id !== activeTicketId) {
      activeTicketId = id
      body = ''; screenshots = []; reason = ''; cancelling = false
      failure = null; fields = {}; feedback = null; changed = false; attempt = null
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
  function start() { failure = null; fields = {}; feedback = null; changed = false }
  async function refresh() {
    try { await invalidateAll() } catch (e) { failure = e }
  }
  async function failed(e: unknown) {
    failure = e
    if (e instanceof ApiError) {
      fields = e.fields
      if (e.status === 403 || e.status === 409) { changed = true; await refresh() }
    }
  }
  async function act(action: Schemas['RequesterAction']) {
    if (busy || !ticket) return
    start(); busy = true
    try {
      await api(data.locale).request('post', '/api/tickets/{id}/actions', { path: { id: ticket.id }, body: action })
      feedback = 'action'; cancelling = false; reason = ''
      await refresh()
    } catch (e) { await failed(e) } finally { busy = false }
  }
  async function send() {
    if (busy || uploading || !ticket || !actions?.reply) return
    start()
    if (!body.trim()) { fields = { body: ['required'] }; return }
    if ([...body].length > 10000) { fields = { body: ['too_long'] }; return }
    busy = true
    const input = { body, screenshot_ids: screenshots }
    attempt = attemptFor(attempt, JSON.stringify({ ticket: ticket.id, ...input }))
    try {
      await api(data.locale).request('post', '/api/tickets/{id}/replies', {
        path: { id: ticket.id }, body: { ...input, idempotency_key: attempt.key },
      })
      body = ''; screenshots = []; attempt = null; feedback = 'reply'
      await refresh()
    } catch (e) { await failed(e) } finally { busy = false }
  }
  async function loadMore() {
    if (loadingMore || !cursor || !ticket) return
    loadingMore = true; failure = null
    try {
      const page = await api(data.locale).request('get', '/api/tickets/{id}/messages', { path: { id: ticket.id }, query: { cursor, limit: 25 } })
      more = [...more, ...page.items]; moreCursor = page.next_cursor
    } catch (e) { failure = e } finally { loadingMore = false }
  }
</script>

<svelte:head><title>{ticket ? `#${ticket.reference} · ${ticket.subject}` : t.myTicketsTitle}</title></svelte:head>
<p><a href="/">{h.back}</a></p>
{#if data.unavailable || !ticket}
  <h1>{h.detail}</h1>
  <Alert>{errorText(t, { code: 'unavailable' })}</Alert>
  <button onclick={refresh}>{h.refresh}</button>
{:else}
  <h1>#{ticket.reference} · {ticket.subject}</h1>
  <div class="actions"><StatusBadge status={ticket.status} label={t.ticketStatuses[ticket.status]} /><button class="secondary" onclick={refresh} disabled={busy}>{h.refresh}</button></div>
  {#if feedback}<Alert kind="success">{feedback === 'reply' ? h.replySent : h.actionSaved}</Alert>{/if}
  {#if changed}<Alert>{h.changed}</Alert>{:else if failure}<Alert>{errorText(t, failure)}</Alert>{/if}
  {#if ticket.closure_reason}<p>{t.closureReasons[ticket.closure_reason]}</p>{/if}

  <dl class="facts">
    <div><dt>{h.team}</dt><dd>{ticket.assignment.team_name ?? (isAiStage(ticket.status) ? h.noHumanTeam : h.generalQueue)}</dd></div>
    <div><dt>{h.assignee}</dt><dd>{ticket.assignment.assignee_display_name ?? h.unassigned}</dd></div>
    <div><dt>{h.priority}</dt><dd>{ticket.priority_name}</dd></div>
    <div><dt>{h.created}</dt><dd><time datetime={ticket.created_at}>{when(ticket.created_at)}</time></dd></div>
  </dl>

  <section aria-labelledby="intake-title">
    <h2 id="intake-title">{h.intake}</h2>
    <p class="hint">{h.frozen}</p>
    <dl class="facts">
      <div><dt>{h.category}</dt><dd>{ticket.intake_snapshot?.catalog.category_name ?? ticket.category_name}</dd></div>
      <div><dt>{h.originalPriority}</dt><dd>{ticket.intake_snapshot?.catalog.priority_name ?? ticket.priority_name}</dd></div>
      {#each ticket.intake_snapshot?.catalog.answers ?? [] as answer (answer.field_id)}<div><dt>{answer.label}</dt><dd class="prose">{answer.value}</dd></div>{/each}
    </dl>
    <p class="prose">{ticket.intake_snapshot?.description ?? ticket.description}</p>
    <ScreenshotGallery ids={ticket.intake_snapshot?.screenshot_ids ?? []} locale={data.locale} />
  </section>

  {#if ticket.ai_solution || ticket.status === 'ai_solved'}
    <section aria-labelledby="ai-title"><h2 id="ai-title">{h.aiSolution}</h2><p class="prose">{ticket.ai_solution ?? h.noSolution}</p></section>
  {/if}
  {#if ticket.status === 'ai_solved'}<Alert kind="info">{h.aiFinal}</Alert>{/if}

  {#if actions?.resolution}
    <section aria-labelledby="resolution-title">
      <h2 id="resolution-title">{h.resolution}</h2>
      {#if proposal}<p class="prose">{publicText(proposal.payload, 'summary')}</p>{/if}
      {#if ticket.confirmation_deadline}<p>{h.deadline} <time datetime={ticket.confirmation_deadline}>{when(ticket.confirmation_deadline)}</time></p>{/if}
      <p class="hint">{h.timeoutHint}</p>
      <div class="actions">
        <button disabled={busy} onclick={() => act({ action: 'accept_resolution', proposal_version: ticket!.proposal_version })}>{h.confirm}</button>
        <button class="secondary" disabled={busy} onclick={() => act({ action: 'reject_resolution', proposal_version: ticket!.proposal_version })}>{h.reject}</button>
      </div>
    </section>
  {/if}
  {#if actions?.reopen}<p><button disabled={busy} onclick={() => act({ action: 'reopen' })}>{h.reopen}</button></p>{/if}
  {#if actions?.cancel}<p><button class="secondary" disabled={busy} onclick={() => (cancelling = !cancelling)}>{h.cancel}</button></p>{/if}
  {#if cancelling}
    <form aria-label={h.cancel} onsubmit={(e) => { e.preventDefault(); act({ action: 'cancel', reason }) }} novalidate>
      <Field name="reason" label={h.reason} hint={h.reasonHint} multiline required bind:value={reason} error={fieldText(t, fields.reason?.[0])} />
      <div class="actions"><button disabled={busy || !actions?.cancel}>{h.confirmCancel}</button><button type="button" class="secondary" disabled={busy} onclick={() => (cancelling = false)}>{h.keep}</button></div>
    </form>
  {/if}

  <section aria-labelledby="conversation-title">
    <h2 id="conversation-title">{h.conversation}</h2>
    {#if !messagesPage.length}<p>{h.noMessages}</p>{/if}
    <ol class="timeline">
      {#each messagesPage as message (message.id)}
        <li><article><h3>{message.author_display_name} · {message.author_kind === 'requester' ? h.requester : h.staff}</h3><time datetime={message.created_at}>{when(message.created_at)}</time><p class="prose">{message.body}</p><ScreenshotGallery ids={message.screenshot_ids} locale={data.locale} /></article></li>
      {/each}
    </ol>
    {#if cursor}<button class="secondary" disabled={loadingMore} onclick={loadMore}>{h.moreMessages}</button>{/if}
    {#if actions?.reply || body || screenshots.length}
      {#if !actions?.reply}<p class="hint">{h.unsentClosed}</p>{/if}
      <form aria-label={h.reply} onsubmit={(e) => { e.preventDefault(); send() }} novalidate>
        <fieldset disabled={busy || !actions?.reply}>
          <legend>{h.reply}</legend>
          <Field name="body" label={h.message} hint={h.appendOnly} multiline required bind:value={body} error={fieldText(t, fields.body?.[0])} />
          <ScreenshotPicker locale={data.locale} bind:ids={screenshots} bind:uploading disabled={busy} />
          {#if fields.screenshot_ids}<p role="alert">{fieldText(t, fields.screenshot_ids[0])}</p>{/if}
          <button disabled={busy || uploading || !actions?.reply}>{h.send}</button>
        </fieldset>
      </form>
    {/if}
  </section>

  <section aria-labelledby="history-title">
    <h2 id="history-title">{h.transitions}</h2>
    <ol class="timeline">
      {#each ticket.transitions as event (event.id)}
        <li>
          <p><strong>{h.events[event.event_type as keyof typeof h.events] ?? t.ticketStatuses[event.to_status]}</strong> · {t.ticketStatuses[event.to_status]}</p>
          <time datetime={event.created_at}>{when(event.created_at)}</time>
          {#if publicText(event.payload, 'summary')}<p class="prose">{publicText(event.payload, 'summary')}</p>{/if}
          {#if publicText(event.payload, 'reason')}<p class="prose">{h.reason}: {publicText(event.payload, 'reason')}</p>{/if}
        </li>
      {/each}
    </ol>
  </section>
{/if}
<style>
  h1 { overflow-wrap: anywhere; }
  .facts { display: flex; flex-wrap: wrap; gap: 1rem 2rem; }
  .facts div { min-width: 8rem; max-width: 100%; }
  dt { color: var(--muted); font-size: .875rem; }
  dd { margin: .25rem 0 0; overflow-wrap: anywhere; }
  .prose { white-space: pre-wrap; overflow-wrap: anywhere; }
  .timeline { list-style: none; padding: 0; }
  .timeline > li { padding: .75rem 1rem; margin: .75rem 0; border-left: 3px solid var(--line); background: var(--surface); }
  h3 { font-size: 1rem; margin: 0 0 .25rem; overflow-wrap: anywhere; }
  time { font-size: .875rem; color: var(--muted); }
  section { margin-top: 1.5rem; }
  fieldset { display: grid; gap: 1rem; }
</style>
