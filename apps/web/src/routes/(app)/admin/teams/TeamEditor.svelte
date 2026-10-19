<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { ApiError, type AccountSummary, type Schemas } from '@tikkit/api-client'
  import { api as apiClient } from '#lib/api.ts'
  import Alert from '#lib/components/Alert.svelte'
  import Field from '#lib/components/Field.svelte'
  import { errorText, fieldText, messages, type Locale } from '#lib/i18n.ts'

  let { team, accounts, locale }: { team?: Schemas['Team']; accounts: AccountSummary[]; locale: Locale } = $props()
  let t = $derived(messages[locale])
  // svelte-ignore state_referenced_locally
  let name = $state(team?.name ?? '')
  // svelte-ignore state_referenced_locally
  let lead = $state(team?.lead_account_id ?? '')
  let eligible = $derived(
    accounts.filter((a) => a.is_active && a.role === 'team_lead' && (!a.is_team_lead || a.team_id === team?.id)),
  )
  // Several editors share the page, so ids carry the team.
  // svelte-ignore state_referenced_locally
  let key = team?.id ?? 'new'
  let error = $state<unknown>(null)
  let fields = $state<Record<string, string[]>>({})
  let busy = $state(false)
  let saved = $state(false)
  async function save(archived = false) {
    if (busy) return
    busy = true
    error = null
    fields = {}
    saved = false
    try {
      const api = apiClient(locale)
      if (team)
        await api.request('put', '/api/teams/{id}', {
          path: { id: team.id },
          body: { name, lead_account_id: archived ? null : lead || null, archived },
        })
      else {
        await api.request('post', '/api/teams', { body: { name, lead_account_id: lead } })
        name = ''
        lead = ''
      }
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

<form
  onsubmit={(e) => {
    e.preventDefault()
    save()
  }}
  novalidate
  aria-busy={busy}
>
  {#if error}<Alert>{errorText(t, error)}</Alert>{/if}
  {#if saved}<Alert kind="success">{t.saved}</Alert>{/if}
  <Field name="team-name-{key}" label={t.teamName} bind:value={name} error={fieldText(t, fields.name?.[0])} />
  <Field
    name="team-lead-{key}"
    label={t.teamLead}
    bind:value={lead}
    hint={t.leadHint}
    options={[
      { value: '', label: t.chooseLead },
      ...eligible.map((a) => ({ value: a.id, label: `${a.username} — ${t.roles[a.role]} (${a.id})` })),
    ]}
    error={fieldText(t, fields.lead_account_id?.[0])}
  />
  <div class="actions">
    <button type="submit" disabled={busy}>{busy ? t.saving : !team ? t.createTeam : team.archived ? t.restoreTeam : t.save}</button>
    {#if team && !team.archived}
      <button type="button" class="secondary" disabled={busy} onclick={() => save(true)}>{t.archiveTeam}</button>
    {/if}
  </div>
  {#if team && !team.archived}<p class="hint">{t.archiveHint}</p>{/if}
</form>

<style>
  form { max-width: 40rem; padding: 1rem 0; }
  p { margin: 0; }
</style>
