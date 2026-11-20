<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { tick, untrack } from 'svelte'
  import type { TicketDetail } from '@tikkit/api-client'
  import { api } from '#lib/api.ts'
  import Alert from '#lib/components/Alert.svelte'
  import { aiAnnouncement, aiFailure, aiStep, type AiAction, type AiFailure, type AiPolicy, type AiStepView } from '#lib/aiStep.ts'
  import { errorText, messages, type Locale } from '#lib/i18n.ts'

  // `busy` is true while a decision request runs; the page pauses its poll and other actions on it.
  let { ticket, policy, locale, disabled = false, busy = $bindable(false) }: {
    ticket: TicketDetail
    policy: AiPolicy | null
    locale: Locale
    disabled?: boolean
    busy?: boolean
  } = $props()
  let t = $derived(messages[locale])
  let a = $derived(t.aiStep)
  let view = $derived(aiStep(ticket))
  let ai = $derived(ticket.ai)
  let reference = $derived(`#${ticket.reference}`)
  let failure = $state<AiFailure | null>(null)
  let announcement = $state('')
  let section = $state<HTMLElement>()
  let heading = $state<HTMLElement>()

  const when = (iso: string) => new Date(iso).toLocaleString(locale, { timeZone: 'UTC', timeZoneName: 'short' })

  // The step changed while the page was open (a poll, a decision, a cancellation): announce it,
  // and move focus to the new heading when it would otherwise be lost with the removed controls.
  let shown: { id: string; kind: AiStepView['kind'] } | undefined
  $effect(() => {
    const next = { id: ticket.id, kind: view.kind }
    const previous = shown
    shown = next
    if (!previous || previous.id !== next.id || previous.kind === next.kind) return
    const key = aiAnnouncement(previous.kind, next.kind)
    untrack(() => {
      announcement = key ? a.announce[key] : ''
      // A failure shown by a decision in flight describes this very change; an older one is stale.
      if (!busy) failure = null
    })
    void keepFocus()
  })
  // Focus goes to the step's heading when it sat on a control that is gone, or nowhere.
  async function keepFocus() {
    await tick()
    const active = document.activeElement
    // Client navigation lands on the page heading (or `#main` from the skip link).
    const landing = document.querySelector('main h1')
    if (!active || active === document.body || active.id === 'main' || active === landing || section?.contains(active)) heading?.focus()
  }

  /**
   * The only place a decision is requested, and only from a click. A repeat of the committed
   * decision is `200` on the server, so a retry after a lost response is safe.
   */
  async function decide(action: AiAction) {
    if (busy || disabled) return
    busy = true
    failure = null
    try {
      await api(locale).request('post', '/api/tickets/{id}/actions', { path: { id: ticket.id }, body: { action } })
      await reload()
    } catch (e) {
      const failed = aiFailure(action, e)
      // A refusal or a lost race is an answer about the current state: show that state.
      if (failed.kind !== 'error') await reload()
      failure = failed
    } finally {
      busy = false
    }
    // The pressed control is usually gone or disabled now (a decision, a refused skip, a second
    // click that landed beside it): continue from the heading unless a usable button holds focus.
    await tick()
    const active = document.activeElement
    if (!(active instanceof HTMLButtonElement && !active.disabled && section?.contains(active))) heading?.focus()
  }
  async function reload() {
    try { await invalidateAll() } catch { /* The page keeps its last state; its poll retries. */ }
  }
</script>

{#snippet suggestionText()}
  {#if ai?.suggestion}
    {#if ai.language !== locale}<p class="hint">{a.asGenerated.replace('{language}', t.languages[ai.language])}</p>{/if}
    <!-- Untrusted model output: text nodes only, never HTML, markdown, or links. -->
    <div class="suggestion" data-testid="ai-suggestion" lang={ai.language}>
      <p class="prose">{ai.suggestion.explanation}</p>
      {#if ai.suggestion.steps.length}<ol>{#each ai.suggestion.steps as step, n (n)}<li class="prose">{step}</li>{/each}</ol>{/if}
    </div>
  {/if}
{/snippet}

{#if view.kind !== 'none'}
  <section class="ai-step" aria-labelledby="ai-step-title" aria-busy={view.kind === 'processing' || busy ? true : undefined} data-ai-step={view.kind} bind:this={section}>
    <p class="sr-only" role="status" data-testid="ai-announcement">{announcement}</p>
    {#if failure}
      <Alert>
        {#if failure.kind === 'skip_forbidden'}{a.skipForbidden}
        {:else if failure.kind === 'conflict'}{a.conflict.replace('{status}', t.ticketStatuses[failure.status ?? ticket.status])}
        {:else}{a.failed} {errorText(t, failure.error)}{/if}
      </Alert>
    {/if}

    {#if view.kind === 'processing'}
      <h2 id="ai-step-title" tabindex="-1" bind:this={heading}>{a.processingTitle}</h2>
      <p class="waiting" role="status"><span class="spinner" aria-hidden="true"></span>{a.processingStatus}</p>
      <p>{a.processingText.replace('{reference}', reference)}</p>
      <p class="hint">{a.processingWait}</p>
      {#if policy?.requester_skip_allowed}
        <div class="actions"><button type="button" class="secondary" disabled={busy || disabled} aria-describedby="ai-skip-hint" onclick={() => decide('skip_ai')}>{a.skip}</button></div>
        <p class="hint" id="ai-skip-hint">{a.skipHint}</p>
      {:else}
        <p class="hint">{a.noSkip}</p>
      {/if}
    {:else if view.kind === 'suggestion'}
      <h2 id="ai-step-title" tabindex="-1" bind:this={heading}>{a.suggestionTitle}</h2>
      <p>{a.suggestionIntro}</p>
      {@render suggestionText()}
      <h3 id="ai-decision-title">{a.question}</h3>
      <div class="actions" role="group" aria-labelledby="ai-decision-title">
        <button type="button" disabled={busy || disabled} aria-describedby="ai-solved-hint" onclick={() => decide('ai_solved')}>{a.solved}</button>
        <button type="button" class="secondary" disabled={busy || disabled} aria-describedby="ai-help-hint" onclick={() => decide('ai_not_solved')}>{a.needHelp}</button>
      </div>
      {#if busy}<p class="waiting" role="status"><span class="spinner" aria-hidden="true"></span>{a.sending}</p>{/if}
      <p class="hint" id="ai-solved-hint">{a.solvedHint}</p>
      <p class="hint" id="ai-help-hint">{a.needHelpHint}</p>
      <p class="hint">{a.noRush}</p>
    {:else if view.kind === 'handoff'}
      <h2 id="ai-step-title" tabindex="-1" bind:this={heading}>{a.handoffTitle}</h2>
      <p data-handoff-reason={view.reason}>{a.handoff[view.reason]}</p>
      <p>{a.handoffKept.replace('{reference}', reference)}</p>
      {#if ai?.suggestion && view.reason === 'not_solved'}
        <h3>{a.tried}</h3>
        {@render suggestionText()}
      {/if}
    {:else if view.kind === 'solved'}
      <h2 id="ai-step-title" tabindex="-1" bind:this={heading}>{a.solvedTitle}</h2>
      <p>{t.history.aiFinal}</p>
      {#if ai?.decided_at}<p>{a.solvedAt} <time datetime={ai.decided_at}>{when(ai.decided_at)}</time></p>{/if}
      <p>{a.solvedNext} <a href="/tickets/new">{t.history.create}</a></p>
      {#if ai?.suggestion}
        <h3>{t.history.aiSolution}</h3>
        {@render suggestionText()}
      {/if}
    {:else}
      <h2 id="ai-step-title" tabindex="-1" bind:this={heading}>{a.title}</h2>
      <p>{a.cancelled}</p>
    {/if}
  </section>
{/if}

<style>
  .ai-step { margin: 1rem 0 1.5rem; padding: 1rem; border: 1px solid var(--line); border-left: 0.375rem solid var(--accent); border-radius: 0.375rem; background: var(--surface); }
  .ai-step[data-ai-step='handoff'], .ai-step[data-ai-step='solved'] { border-left-color: var(--ok); }
  h2 { margin-top: 0; overflow-wrap: anywhere; }
  h3 { font-size: 1rem; margin: 1rem 0 0.5rem; }
  .waiting { display: flex; gap: 0.5rem; align-items: center; font-weight: 600; }
  .waiting .spinner { flex: none; }
  .suggestion { padding: 0.75rem 1rem; border: 1px solid var(--line); border-radius: 0.375rem; }
  .suggestion ol { margin: 0.5rem 0 0; padding-left: 1.5rem; }
  .suggestion li { margin: 0.25rem 0; }
  .prose { white-space: pre-wrap; overflow-wrap: anywhere; }
  .actions button { min-width: 10rem; }
  @media (max-width: 40rem) {
    .actions button { width: 100%; }
  }
</style>
