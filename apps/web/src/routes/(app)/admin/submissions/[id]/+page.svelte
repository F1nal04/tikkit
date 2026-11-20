<script lang="ts">
  import Alert from '#lib/components/Alert.svelte'
  import StatusBadge from '#lib/components/StatusBadge.svelte'
  import { messages } from '#lib/i18n.ts'
  import { publicText } from '#lib/requester.ts'
  import ScreenshotGallery from '#lib/ScreenshotGallery.svelte'
  import { inHelpDesk, isAiResolution } from '#lib/submissions.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let s = $derived(t.submissions)
  let d = $derived(data.submission)
  let summary = $derived(d.summary)
  let ai = $derived(d.ai)
  let intake = $derived(d.intake_snapshot)
  const date = $derived(new Intl.DateTimeFormat(data.locale, { dateStyle: 'medium', timeStyle: 'short', timeZone: 'UTC' }))
  const when = (value: string) => `${date.format(new Date(value))} UTC`
  // Attempt codes first, then the provider and budget codes of the usage report; unknown codes show as they are.
  const codeLabel = (code: string) => ({ ...t.ai.usage.outcomeLabels, ...t.ai.records.codes } as Record<string, string>)[code] ?? code
  const eventLabel = (type: string) =>
    ({ ...t.history.events, ...s.eventLabels } as Record<string, string>)[type] ?? type
  const provider = (name: string) => t.ai.providers[name as keyof typeof t.ai.providers] ?? name
</script>

<svelte:head><title>#{summary.reference} · {summary.subject}</title></svelte:head>

<p><a href="/admin/submissions">{s.back}</a></p>
<h1>#{summary.reference} · {summary.subject}</h1>
{#if d.content_removed_at}<Alert kind="info">{t.retention.contentRemoved}</Alert>{:else if d.ai?.content_removed}<Alert kind="info">{t.retention.aiContentRemoved}</Alert>{/if}
<div class="badges">
  <StatusBadge status={summary.status} label={t.statuses[summary.status]} />
  <span data-testid="resolution" data-resolution={summary.resolution}>{s.resolutions[summary.resolution]}</span>
</div>
<p class="hint">{s.readOnly}</p>
{#if ai}
  <div data-testid="ai-notice" data-outcome={ai.outcome}>
    <Alert kind={isAiResolution(ai.outcome) ? 'success' : 'info'}>{s.notices[ai.outcome]}</Alert>
  </div>
{/if}
{#if inHelpDesk(summary)}<p><a href={`/queue/${summary.id}`}>{s.staffView}</a></p>{/if}

<section aria-labelledby="facts-title">
  <h2 id="facts-title">{s.facts}</h2>
  <dl class="facts">
    <div><dt>{s.requester}</dt><dd>{summary.requester.display_name} ({summary.requester.username})</dd></div>
    <div><dt>{s.category}</dt><dd>{summary.category_name}</dd></div>
    <div><dt>{s.priority}</dt><dd>{summary.priority_name}</dd></div>
    <div><dt>{s.team}</dt><dd>{summary.team_name ?? (inHelpDesk(summary) ? s.generalQueue : s.notHuman)}</dd></div>
    <div><dt>{s.assignee}</dt><dd>{summary.assignee_display_name ?? s.unassigned}</dd></div>
    <div><dt>{s.route}</dt><dd>{summary.route ? s.routes[summary.route] : '—'}</dd></div>
    <div><dt>{s.aiOutcome}</dt><dd>{summary.ai_outcome ? s.aiOutcomes[summary.ai_outcome] : s.noAi}</dd></div>
    <div><dt>{s.source}</dt><dd>{summary.resolution_source ? s.sources[summary.resolution_source] : s.sources.none}</dd></div>
    {#if summary.closure_reason}<div><dt>{s.resolution}</dt><dd>{t.closureReasons[summary.closure_reason]}</dd></div>{/if}
    {#if d.confirmation_deadline}<div><dt>{s.deadline}</dt><dd><time datetime={d.confirmation_deadline}>{when(d.confirmation_deadline)}</time></dd></div>{/if}
    <div><dt>{s.submitted}</dt><dd><time datetime={summary.created_at}>{when(summary.created_at)}</time></dd></div>
    <div><dt>{s.updated}</dt><dd><time datetime={summary.updated_at}>{when(summary.updated_at)}</time></dd></div>
  </dl>
</section>

<section aria-labelledby="intake-title">
  <h2 id="intake-title">{s.intake}</h2>
  <p class="hint">{s.frozen}</p>
  <dl class="facts">
    <div><dt>{s.category}</dt><dd>{intake?.catalog.category_name ?? summary.category_name}</dd></div>
    <div><dt>{s.originalPriority}</dt><dd>{intake?.catalog.priority_name ?? summary.priority_name}</dd></div>
    {#each intake?.catalog.answers ?? [] as answer (answer.field_id)}<div><dt>{answer.label}</dt><dd class="prose">{answer.value}</dd></div>{/each}
  </dl>
  <h3>{s.description}</h3>
  <p class="prose" data-testid="description">{intake?.description ?? d.description}</p>
  <ScreenshotGallery ids={intake?.screenshot_ids ?? []} locale={data.locale} />
    {#if d.removed_screenshots > 0}<p class="hint" data-testid="screenshots-removed">{t.retention.screenshotsRemoved.replace('{count}', String(d.removed_screenshots))}</p>{/if}
</section>

{#if ai}
  <section aria-labelledby="ai-title">
    <h2 id="ai-title">{s.ai}</h2>
    <dl class="facts">
      <div><dt>{s.aiOutcome}</dt><dd>{s.aiOutcomes[ai.outcome]}</dd></div>
      {#if ai.outcome_code}<div><dt>{s.reason}</dt><dd>{codeLabel(ai.outcome_code)}</dd></div>{/if}
      <div><dt>{s.model}</dt><dd>{provider(ai.provider)} · {ai.model}</dd></div>
      <div><dt>{s.language}</dt><dd>{t.languages[ai.language]}</dd></div>
      <div><dt>{s.started}</dt><dd><time datetime={ai.created_at}>{when(ai.created_at)}</time></dd></div>
      {#if ai.finished_at}<div><dt>{s.finished}</dt><dd><time datetime={ai.finished_at}>{when(ai.finished_at)}</time></dd></div>{/if}
      <div><dt>{s.decision}</dt><dd data-testid="decision">{ai.decision ? s.decisions[ai.decision] : s.noDecision}</dd></div>
      {#if ai.decided_at}<div><dt>{s.decidedAt}</dt><dd><time datetime={ai.decided_at}>{when(ai.decided_at)}</time></dd></div>{/if}
      {#if ai.decision === 'solved' && ai.decision_event_id}<div><dt>{s.confirmationEvent}</dt><dd><a href={`#event-${ai.decision_event_id}`}>#{ai.decision_event_id}</a></dd></div>{/if}
    </dl>
    <h3>{s.suggestion}</h3>
    {#if ai.suggestion}
      <!-- Untrusted model output: plain text in the language it was written in. -->
      <div lang={ai.language} data-testid="suggestion">
        <p class="prose">{ai.suggestion.explanation}</p>
        <ol>{#each ai.suggestion.steps as step, i (i)}<li class="prose">{step}</li>{/each}</ol>
      </div>
    {:else}
      <p>{s.noSuggestion}</p>
    {/if}
  </section>
{/if}

<section aria-labelledby="history-title">
  <h2 id="history-title">{s.history}</h2>
  <ol class="timeline">
    {#each d.events as event (event.id)}
      <li id={`event-${event.id}`} data-event={event.event_type}>
        <p>
          <strong>{eventLabel(event.event_type)}</strong>
          {#if event.from_status && event.to_status} · {t.statuses[event.from_status]} → {t.statuses[event.to_status]}{/if}
        </p>
        <p class="meta">
          #{event.id} · {event.actor ? `${event.actor.display_name} (${event.actor.username})` : s.system} ·
          <time datetime={event.created_at}>{when(event.created_at)}</time>
        </p>
        {#if publicText(event.payload, 'summary')}<p class="prose">{s.summary}: {publicText(event.payload, 'summary')}</p>{/if}
        {#if event.event_type === 'cancel' && publicText(event.payload, 'reason')}<p class="prose">{s.cancelReason}: {publicText(event.payload, 'reason')}</p>{/if}
      </li>
    {/each}
  </ol>
</section>

<style>
  h1 { overflow-wrap: anywhere; }
  p { margin: 0; }
  .badges { display: flex; flex-wrap: wrap; align-items: center; gap: .5rem 1rem; }
  .hint, .meta { color: var(--muted); font-size: .875rem; }
  .facts { display: flex; flex-wrap: wrap; gap: 1rem 2rem; }
  .facts div { min-width: 8rem; max-width: 100%; }
  dt { color: var(--muted); font-size: .875rem; }
  dd { margin: .25rem 0 0; overflow-wrap: anywhere; }
  .prose { white-space: pre-wrap; overflow-wrap: anywhere; }
  .timeline { list-style: none; padding: 0; }
  .timeline > li { display: grid; gap: .25rem; padding: .75rem 1rem; margin: .75rem 0; border-left: 3px solid var(--line); background: var(--surface); }
  .timeline > li:target { border-left-color: var(--accent); }
  h3 { font-size: 1rem; margin: 1rem 0 .25rem; }
  section { margin-top: 1.5rem; }
</style>
