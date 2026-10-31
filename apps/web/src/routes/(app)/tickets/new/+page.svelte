<script lang="ts">
  import { invalidate } from '$app/navigation'
  import { tick, untrack } from 'svelte'
  import { ApiError, type Schemas } from '@tikkit/api-client'
  import Alert from '#lib/components/Alert.svelte'
  import Dialog from '#lib/components/Dialog.svelte'
  import EmptyState from '#lib/components/EmptyState.svelte'
  import Field from '#lib/components/Field.svelte'
  import StatusBadge from '#lib/components/StatusBadge.svelte'
  import ScreenshotPicker from '#lib/ScreenshotPicker.svelte'
  import { api as apiClient } from '#lib/api.ts'
  import { errorText, fieldText, messages } from '#lib/i18n.ts'
  import {
    attemptFor,
    emptyForm,
    hiddenAnswers,
    isEmpty,
    OTHER,
    restore,
    sameIntake,
    toIntake,
    validate,
    visibleFields,
    type Attempt,
    type Restored,
  } from '#lib/intake.ts'
  import type { PageProps } from './$types'

  type Draft = Schemas['Draft']

  const AUTOSAVE_MS = 1200

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let f = $derived(t.ticketForm)
  let catalog = $derived(data.catalog)
  const api = () => apiClient(data.locale)

  let form = $state(emptyForm())
  let errors = $state<Record<string, string[]>>({})
  let formError = $state<unknown>(null)
  let notices = $state<Pick<Restored, 'categoryGone' | 'priorityGone' | 'dropped'>>({ categoryGone: false, priorityGone: false, dropped: [] })
  let submitting = $state(false)
  let receipt = $state<(Schemas['SubmissionReceipt'] & { category: string; priority: string }) | null>(null)
  let receiptHeading = $state<HTMLElement>()

  // Draft autosave. `draft` is the saved copy's identity and version; `stale` means another tab or
  // device changed or removed it, so nothing is written until the user decides.
  // svelte-ignore state_referenced_locally
  let drafts = $state<Draft[]>(data.drafts)
  let draft = $state<{ id: string; version: number } | null>(null)
  let saveState = $state<'idle' | 'saving' | 'saved' | 'failed'>('idle')
  let savedAt = $state<Date | null>(null)
  let stale = $state<'changed' | 'gone' | null>(null)
  let discarding = $state<Draft | null>(null)
  let discardOpen = $state(false)
  let timer: ReturnType<typeof setTimeout> | undefined
  let queue: Promise<unknown> = Promise.resolve()
  let attempt: Attempt | null = null
  // The draft identity the save queue reads. A plain value, because a response can still arrive
  // after navigation and the next queued save needs the version even though the page is gone.
  let savedDraft: { id: string; version: number } | null = null
  let alive = true
  // True when the saved draft still holds answers the visible form dropped. Submit writes the
  // visible intake first; ordinary autosave waits for the next edit, so resume does not destroy them.
  let serverDiffers = false
  let forceSync = false
  let saveError: unknown = null

  let visible = $derived(visibleFields(catalog, form.category_id))
  let hidden = $derived(hiddenAnswers(catalog, form))
  let category = $derived(catalog.categories.find((c) => c.id === form.category_id))
  let intake = $derived(toIntake(form, catalog))
  let json = $derived(JSON.stringify(intake))
  // svelte-ignore state_referenced_locally
  let lastSaved = JSON.stringify(intake)
  let available = $derived(catalog.categories.length > 0 && catalog.priorities.length > 0)

  // Every field has an answer slot to bind to, including after the catalog is refreshed.
  $effect.pre(() => {
    for (const field of catalog.fields) form.answers[field.id] ??= { choice: '', text: '' }
  })

  $effect(() => {
    if (!alive) return
    if (json !== untrack(() => lastSaved)) untrack(schedule)
    else clearTimeout(timer)
    return () => clearTimeout(timer)
  })

  const schedule = () => {
    if (!alive) return
    clearTimeout(timer)
    timer = setTimeout(() => {
      if (alive) flush()
    }, AUTOSAVE_MS)
  }
  // Saves run one at a time, each with the then-current form and version, so a slow response
  // cannot overwrite a newer one.
  const flush = () => (queue = queue.then(save, save))

  function setDraft(next: { id: string; version: number } | null) {
    savedDraft = next
    if (alive) draft = next
  }

  function track(saved: Draft) {
    setDraft({ id: saved.id, version: saved.version })
    if (!alive) return
    drafts = [saved, ...drafts.filter((d) => d.id !== saved.id)]
    savedAt = new Date(saved.updated_at)
    saveState = 'saved'
  }

  async function save(): Promise<boolean> {
    clearTimeout(timer)
    if (stale) return false
    const body = untrack(() => intake)
    const snapshot = JSON.stringify(body)
    const sync = forceSync
    forceSync = false
    const current = savedDraft
    if ((snapshot === lastSaved && !sync) || (!current && isEmpty(body))) return true
    if (alive) saveState = 'saving'
    try {
      track(
        current
          ? await api().request('patch', '/api/ticket-drafts/{id}', { path: { id: current.id }, body: { version: current.version, intake: body } })
          : await api().request('post', '/api/ticket-drafts', { body }),
      )
      lastSaved = snapshot
      serverDiffers = false
      saveError = null
      return true
    } catch (e) {
      if (!alive) return false
      saveError = e
      // The forced write did not land; the next submit has to try again.
      if (sync) forceSync = true
      if (e instanceof ApiError && (e.code === 'conflict' || e.code === 'not_found')) {
        stale = e.code === 'conflict' ? 'changed' : 'gone'
        saveState = 'idle'
      } else {
        saveState = 'failed'
      }
      return false
    }
  }

  // Client-only: leaving the page saves a debounced edit. beforeunload does not fire for in-app navigation.
  $effect(() => () => {
    alive = false
    clearTimeout(timer)
    void flush().catch(() => {})
  })

  function load(saved: Draft) {
    const restored = restore(saved.intake, catalog)
    form = restored.form
    notices = restored
    errors = {}
    formError = null
    stale = null
    attempt = null
    setDraft({ id: saved.id, version: saved.version })
    savedAt = new Date(saved.updated_at)
    saveState = 'saved'
    // The restored form is what the user sees. Saving waits for their next edit so a draft is
    // never rewritten without them; submit writes first, because the draft endpoint submits the saved copy.
    const cleaned = toIntake(form, catalog)
    lastSaved = JSON.stringify(cleaned)
    serverDiffers = !sameIntake(saved.intake, cleaned)
  }

  async function resume(id: string) {
    await flush()
    if (stale) return
    try {
      load(await api().request('get', '/api/ticket-drafts/{id}', { path: { id } }))
    } catch (e) {
      drafts = drafts.filter((d) => d.id !== id)
      formError = e
    }
  }

  async function loadSaved() {
    try {
      load(await api().request('get', '/api/ticket-drafts/{id}', { path: { id: draft!.id } }))
    } catch (e) {
      formError = e
    }
  }

  async function keepMine() {
    setDraft(null)
    stale = null
    serverDiffers = false
    forceSync = false
    lastSaved = ''
    await flush()
  }

  function askDiscard(target: Draft) {
    discarding = target
    discardOpen = true
  }

  async function discard() {
    const target = discarding
    discardOpen = false
    if (!target) return
    try {
      await api().request('delete', '/api/ticket-drafts/{id}', { path: { id: target.id }, query: { version: target.version } })
    } catch (e) {
      // A newer version or an already-gone draft: refresh the list rather than guess.
      if (!(e instanceof ApiError && e.code === 'not_found')) {
        formError = e
        return
      }
    }
    drafts = drafts.filter((d) => d.id !== target.id)
    if (savedDraft?.id === target.id) reset()
  }

  function reset() {
    clearTimeout(timer)
    form = emptyForm()
    notices = { categoryGone: false, priorityGone: false, dropped: [] }
    errors = {}
    formError = null
    stale = null
    setDraft(null)
    serverDiffers = false
    forceSync = false
    attempt = null
    receipt = null
    saveState = 'idle'
    lastSaved = JSON.stringify(toIntake(form, catalog))
  }

  const errorFor = (key: string) => fieldText(t, errors[key]?.[0])

  // The first invalid input in reading order, whichever order the errors arrived in.
  async function focusFirstError() {
    await tick()
    document.querySelector<HTMLElement>('form [aria-invalid="true"]')?.focus()
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault()
    // Nothing else runs while a submission is in flight, so a double click or Enter cannot repeat it.
    if (submitting) return
    formError = null
    errors = validate(form, catalog)
    if (Object.keys(errors).length) return focusFirstError()
    submitting = true
    let focusErrors = false
    try {
      // The draft endpoint submits the saved copy. Write the visible form first when resume
      // dropped answers, so they are not submitted and the idempotency key matches that write.
      if (serverDiffers) forceSync = true
      if (!(await flush())) {
        forceSync = false
        if (!stale && alive) {
          if (saveError instanceof ApiError && saveError.code === 'validation_failed') {
            errors = saveError.fields
            focusErrors = true
          } else formError = saveError ?? new ApiError(0, null)
        }
      } else if (alive) {
        const body = untrack(() => intake)
        const current = savedDraft
        const fingerprint = `${current?.id}:${current?.version}:${JSON.stringify(body)}`
        attempt = attemptFor(attempt, fingerprint)
        const sent = current
          ? await api().request('post', '/api/ticket-drafts/{id}/submit', {
              path: { id: current.id },
              body: { version: current.version, idempotency_key: attempt.key },
            })
          : await api().request('post', '/api/tickets', { body: { intake: body, idempotency_key: attempt.key } })
        if (!alive) return
        receipt = {
          ...sent,
          category: category?.name ?? '',
          priority: catalog.priorities.find((p) => p.id === form.priority_id)?.name ?? '',
        }
        if (current) drafts = drafts.filter((d) => d.id !== current.id)
        setDraft(null)
        serverDiffers = false
        clearTimeout(timer)
        await tick()
        receiptHeading?.focus()
      }
    } catch (e) {
      if (!alive) return
      if (e instanceof ApiError && e.code === 'validation_failed') {
        errors = e.fields
        // A category, priority, or choice archived since this page loaded: refresh the catalog, keep the input.
        if (Object.keys(errors).some((k) => k === 'category_id' || k === 'priority_id' || k.startsWith('answers.'))) {
          await invalidate('tikkit:catalog')
        }
        focusErrors = true
      } else if (e instanceof ApiError && savedDraft && (e.code === 'conflict' || e.code === 'not_found')) {
        stale = e.code === 'conflict' ? 'changed' : 'gone'
      } else {
        formError = e
      }
    } finally {
      if (alive) submitting = false
    }
    // After the fieldset is enabled again; focusing a control inside a disabled fieldset does nothing.
    if (alive && focusErrors) await focusFirstError()
  }

  const time = (d: Date) => new Intl.DateTimeFormat(data.locale, { timeStyle: 'short' }).format(d)
  const names = (list: { label: string }[]) => list.map((x) => x.label).join(', ')
</script>

<svelte:head><title>{t.newTicketTitle}</title></svelte:head>
<svelte:window onbeforeunload={(e) => (json !== lastSaved || saveState === 'saving') && !receipt && e.preventDefault()} />

<h1>{t.newTicketTitle}</h1>

{#if receipt}
  <Alert kind="success">
    <h2 tabindex="-1" bind:this={receiptHeading}>{f.sentTitle}</h2>
    <p>{f.sentText.replace('{reference}', `#${receipt.reference}`)}</p>
    <p>{f.received[receipt.status as keyof typeof f.received] ?? f.received.queued}</p>
    <p><StatusBadge status={receipt.status} label={t.statuses[receipt.status]} /></p>
    <dl>
      <dt>{f.receiptCategory}</dt>
      <dd>{receipt.category}</dd>
      <dt>{f.receiptPriority}</dt>
      <dd>{receipt.priority}</dd>
    </dl>
    <p><a href="/tickets/{receipt.id}">{f.openTicket}</a></p>
  </Alert>
  <p><button type="button" class="secondary" onclick={reset}>{f.another}</button></p>
{:else if !available}
  <EmptyState title={t.newTicketEmpty}>{f.unavailable}</EmptyState>
{:else}
  <p>{f.intro}</p>
  <p class="who">{f.submittingAs} <strong>{data.user?.display_name}</strong> (<span>{data.user?.username}</span>)</p>

  {#if drafts.some((d) => d.id !== draft?.id)}
    <section aria-labelledby="drafts-title" class="drafts">
      <h2 id="drafts-title">{f.drafts}</h2>
      <p class="hint">{f.draftsHint}</p>
      <ul>
        {#each drafts.filter((d) => d.id !== draft?.id) as saved (saved.id)}
          <li>
            <span class="draft-title">{saved.intake.subject?.trim() || f.noSubject}</span>
            <span class="hint">{f.lastSaved} <time datetime={saved.updated_at}>{new Intl.DateTimeFormat(data.locale, { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(saved.updated_at))}</time></span>
            <button type="button" class="secondary" onclick={() => resume(saved.id)} aria-label="{f.resume}: {saved.intake.subject?.trim() || f.noSubject}">{f.resume}</button>
            <button type="button" class="secondary" onclick={() => askDiscard(saved)} aria-label="{f.discard}: {saved.intake.subject?.trim() || f.noSubject}">{f.discard}</button>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if stale}
    <Alert kind="error">
      <strong>{f.stale}</strong> {f.staleText}
      <p class="row">
        {#if stale === 'changed'}<button type="button" class="secondary" onclick={loadSaved}>{f.loadSaved}</button>{/if}
        <button type="button" class="secondary" onclick={keepMine}>{f.keepMine}</button>
      </p>
    </Alert>
  {/if}
  {#if notices.categoryGone}<Alert kind="info">{f.categoryGone}</Alert>{/if}
  {#if notices.priorityGone}<Alert kind="info">{f.priorityGone}</Alert>{/if}
  {#if notices.dropped.length}
    <Alert kind="info">
      {f.droppedAnswers}
      <ul>
        {#each notices.dropped as item, i (i)}<li>{item.label ? `${item.label}: ` : ''}{item.text ?? ''}</li>{/each}
      </ul>
    </Alert>
  {/if}
  {#if formError}<Alert kind="error">{errorText(t, formError)}</Alert>{/if}
  {#if Object.keys(errors).length}<Alert kind="error">{t.errors.validation_failed}</Alert>{/if}

  <form novalidate onsubmit={submit} aria-busy={submitting}>
    <p class="form-state">
      <StatusBadge status="draft" label={t.statuses.draft} />
      {#if saveState === 'saving'}{f.stateSaving}
      {:else if saveState === 'failed'}<span role="status">{f.stateFailed}</span>
      {:else if saveState === 'saved' && savedAt}{f.stateSaved.replace('{time}', time(savedAt))}
      {:else}{f.stateUnsaved}{/if}
    </p>

    <fieldset disabled={submitting}>
      <Field
        label={f.category}
        name="category_id"
        required
        bind:value={form.category_id}
        error={errorFor('category_id')}
        options={[{ value: '', label: f.categoryPlaceholder }, ...catalog.categories.map((c) => ({ value: c.id, label: c.name }))]}
      />

      {#if visible.length}
        <div class="conditional">
          {#each visible as field (field.id)}
            {@const key = `answers.${field.id}`}
            {@const answer = form.answers[field.id]}
            {#if answer}
              {#if field.kind === 'select'}
                <Field
                  label={field.label}
                  name="answer-{field.id}"
                  required={field.required}
                  hint={field.guidance || undefined}
                  bind:value={answer.choice}
                  error={answer.choice === OTHER ? undefined : errorFor(key)}
                  options={[
                    { value: '', label: f.choose },
                    ...field.choices.map((c) => ({ value: c.id, label: c.label })),
                    ...(field.allow_other ? [{ value: OTHER, label: f.otherChoice }] : []),
                  ]}
                />
                {#if answer.choice === OTHER}
                  <Field
                    label={f.otherAnswer.replace('{label}', field.label)}
                    name="answer-{field.id}-text"
                    required={field.required}
                    bind:value={answer.text}
                    error={errorFor(key)}
                  />
                {/if}
              {:else}
                <Field
                  label={field.label}
                  name="answer-{field.id}"
                  required={field.required}
                  hint={field.guidance || undefined}
                  bind:value={answer.text}
                  error={errorFor(key)}
                />
              {/if}
            {/if}
          {/each}
        </div>
      {/if}
      {#if hidden.length}<p class="hint">{f.hiddenAnswers.replace('{names}', names(hidden))}</p>{/if}

      <Field label={f.subject} name="subject" required autocomplete="off" bind:value={form.subject} error={errorFor('subject')} />
      <Field label={f.description} name="description" required multiline hint={f.descriptionHint} bind:value={form.description} error={errorFor('description')} />
      <Field
        label={f.priority}
        name="priority_id"
        required
        hint={f.priorityHint}
        bind:value={form.priority_id}
        error={errorFor('priority_id')}
        options={[{ value: '', label: f.priorityPlaceholder }, ...catalog.priorities.map((p) => ({ value: p.id, label: p.name }))]}
      />

      <ScreenshotPicker locale={data.locale} bind:ids={form.screenshot_ids} error={errorFor('screenshot_ids')} />
      <p class="hint">{category?.skip_ai ? f.skipsAi : f.aiNote}</p>
    </fieldset>

    <div class="form-actions">
      <button type="submit" disabled={submitting}>{submitting ? f.submitting : f.submit}</button>
    </div>
  </form>

  <Dialog bind:open={discardOpen} title={f.discardTitle}>
    <p>{f.discardText}</p>
    {#snippet actions()}
      <button type="button" class="secondary" onclick={() => (discardOpen = false)}>{t.cancel}</button>
      <button type="button" onclick={discard}>{f.discard}</button>
    {/snippet}
  </Dialog>
{/if}

<style>
  .drafts ul {
    display: grid;
    gap: 0.5rem;
    padding: 0;
    list-style: none;
  }
  .drafts li,
  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    align-items: center;
  }
  .draft-title {
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  fieldset {
    display: grid;
    gap: 1rem;
    min-width: 0;
    padding: 0;
    border: 0;
  }
  .conditional {
    display: grid;
    gap: 1rem;
    padding-left: 0.75rem;
    border-left: 0.25rem solid var(--line);
  }
  .form-actions button {
    min-width: 12rem;
  }
  @media (max-width: 40rem) {
    .form-actions button {
      width: 100%;
    }
  }
</style>
