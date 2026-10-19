<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { ApiError, createClient } from '@tikkit/api-client'
  import { errorText, fieldText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'
  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  // svelte-ignore state_referenced_locally
  let form = $state({ ...data.settings })
  let busy = $state(false)
  let saved = $state(false)
  let error = $state<string | null>(null)
  let fields = $state<Record<string, string[]>>({})
  async function save(event: SubmitEvent) {
    event.preventDefault()
    if (busy) return
    busy = true; saved = false; error = null; fields = {}
    try {
      await createClient().request('put', '/api/admin/settings', { body: form })
      await invalidateAll()
      saved = true
    } catch (e) {
      error = errorText(t, e)
      if (e instanceof ApiError) fields = e.fields
    } finally { busy = false }
  }
</script>
<svelte:head><title>{t.settingsTitle}</title></svelte:head>
<main class="wide admin-page">
  <h1>{t.settingsTitle}</h1>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if saved}<p role="status">{t.saved}</p>{/if}
  <form onsubmit={save} novalidate aria-busy={busy}>
    <label>{t.organizationName}<input bind:value={form.name} maxlength="120" aria-invalid={!!fields.name} aria-describedby={fields.name ? 'name-error' : undefined} /></label>
    {#if fields.name}<span class="error" id="name-error">{fieldText(t, fields.name[0])}</span>{/if}
    <label>{t.timezone}<input bind:value={form.timezone} placeholder="Europe/Berlin" aria-invalid={!!fields.timezone} aria-describedby="timezone-hint timezone-error" /></label>
    <span class="hint" id="timezone-hint">{t.timezoneHint}</span>
    <span class="error" id="timezone-error">{fieldText(t, fields.timezone?.[0]) ?? ''}</span>
    <label>{t.defaultLanguage}<select bind:value={form.default_language}><option value="en">{t.languages.en}</option><option value="de">{t.languages.de}</option></select></label>
    <fieldset>
      <legend>{t.ticketPolicies}</legend>
      <div class="policies">
        <label class="check"><input type="checkbox" bind:checked={form.allow_reopening} />{t.allowReopening}</label>
        <p class="hint">{t.reopeningHint}</p>
        <label>{t.cancellationPolicy}<select bind:value={form.requester_cancellation_policy}>
          <option value="anytime_while_open">{t.cancellationPolicies.anytime_while_open}</option>
          <option value="before_work_starts">{t.cancellationPolicies.before_work_starts}</option>
          <option value="disabled">{t.cancellationPolicies.disabled}</option>
        </select></label>
        <p class="hint">{t.cancellationHint}</p>
        <label>{t.confirmationHours}<input type="number" min="1" step="1" bind:value={form.resolution_confirmation_hours} aria-invalid={!!fields.resolution_confirmation_hours} aria-describedby="confirmation-hint confirmation-error" /></label>
        <p class="hint" id="confirmation-hint">{t.confirmationHint}</p>
        <span class="error" id="confirmation-error">{fieldText(t, fields.resolution_confirmation_hours?.[0]) ?? ''}</span>
        <label class="check"><input type="checkbox" bind:checked={form.allow_requester_ai_skip} />{t.allowAiSkip}</label>
        <p class="hint">{t.aiSkipHint}</p>
      </div>
    </fieldset>
    <button type="submit" disabled={busy}>{busy ? t.saving : t.save}</button>
  </form>
</main>
<style>.policies { display: grid; gap: .75rem; } p { margin: 0 0 .75rem; } form { max-width: 42rem; }</style>
