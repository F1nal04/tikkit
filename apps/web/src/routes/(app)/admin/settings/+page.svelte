<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { ApiError } from '@tikkit/api-client'
  import { api as apiClient } from '#lib/api.ts'
  import Alert from '#lib/components/Alert.svelte'
  import Field from '#lib/components/Field.svelte'
  import { errorText, fieldText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  // Initial values only; the inputs own them afterwards, so a rejected save keeps what was typed.
  // svelte-ignore state_referenced_locally
  let form = $state({ ...data.settings })
  // svelte-ignore state_referenced_locally
  let hours = $state(String(data.settings.resolution_confirmation_hours))
  let busy = $state(false)
  let saved = $state(false)
  let error = $state<unknown>(null)
  let fields = $state<Record<string, string[]>>({})

  async function save(event: SubmitEvent) {
    event.preventDefault()
    if (busy) return
    busy = true
    saved = false
    error = null
    fields = {}
    try {
      await apiClient(data.locale).request('put', '/api/admin/settings', {
        body: { ...form, resolution_confirmation_hours: Number(hours) },
      })
      await invalidateAll()
      saved = true
    } catch (e) {
      error = e
      if (e instanceof ApiError) fields = e.fields
    } finally {
      busy = false
    }
  }
</script>

<svelte:head><title>{t.settingsTitle}</title></svelte:head>

<h1>{t.settingsTitle}</h1>
{#if error}<Alert>{errorText(t, error)}</Alert>{/if}
{#if saved}<Alert kind="success">{t.saved}</Alert>{/if}
<form onsubmit={save} novalidate aria-busy={busy}>
  <Field name="name" label={t.organizationName} bind:value={form.name} error={fieldText(t, fields.name?.[0])} />
  <Field
    name="timezone"
    label={t.timezone}
    bind:value={form.timezone}
    hint={t.timezoneHint}
    error={fieldText(t, fields.timezone?.[0])}
  />
  <Field
    name="default_language"
    label={t.defaultLanguage}
    bind:value={form.default_language}
    options={[{ value: 'en', label: t.languages.en }, { value: 'de', label: t.languages.de }]}
  />
  <fieldset>
    <legend>{t.ticketPolicies}</legend>
    <div class="policies">
      <label class="check"><input type="checkbox" bind:checked={form.allow_reopening} />{t.allowReopening}</label>
      <p class="hint">{t.reopeningHint}</p>
      <Field
        name="requester_cancellation_policy"
        label={t.cancellationPolicy}
        bind:value={form.requester_cancellation_policy}
        hint={t.cancellationHint}
        options={(['anytime_while_open', 'before_work_starts', 'disabled'] as const).map((p) => ({
          value: p,
          label: t.cancellationPolicies[p],
        }))}
      />
      <Field
        name="resolution_confirmation_hours"
        type="number"
        label={t.confirmationHours}
        bind:value={hours}
        hint={t.confirmationHint}
        error={fieldText(t, fields.resolution_confirmation_hours?.[0])}
      />
      <label class="check"><input type="checkbox" bind:checked={form.allow_requester_ai_skip} />{t.allowAiSkip}</label>
      <p class="hint">{t.aiSkipHint}</p>
    </div>
  </fieldset>
  <div class="actions"><button type="submit" disabled={busy}>{busy ? t.saving : t.save}</button></div>
</form>

<style>
  .policies { display: grid; gap: 0.75rem; }
  p { margin: 0; }
  form { max-width: 42rem; }
</style>
