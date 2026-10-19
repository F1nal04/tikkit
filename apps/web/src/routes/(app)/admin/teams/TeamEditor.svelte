<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { ApiError, createClient, type AccountSummary, type Schemas } from '@tikkit/api-client'
  import { errorText, fieldText, messages, type Locale } from '#lib/i18n.ts'
  let { team, accounts, locale }: { team?: Schemas['Team']; accounts: AccountSummary[]; locale: Locale } = $props()
  let t = $derived(messages[locale])
  // svelte-ignore state_referenced_locally
  let name = $state(team?.name ?? '')
  // svelte-ignore state_referenced_locally
  let lead = $state(team?.lead_account_id ?? '')
  let eligible = $derived(accounts.filter((a) => a.is_active && a.role === 'team_lead' && (!a.is_team_lead || a.team_id === team?.id)))
  let error = $state<string | null>(null)
  let fields = $state<Record<string, string[]>>({})
  let busy = $state(false)
  let saved = $state(false)
  async function save(archived = false) {
    if (busy) return
    busy = true; error = null; fields = {}; saved = false
    try {
      const api = createClient()
      if (team) await api.request('put', '/api/teams/{id}', { path: { id: team.id }, body: { name, lead_account_id: archived ? null : lead || null, archived } })
      else { await api.request('post', '/api/teams', { body: { name, lead_account_id: lead } }); name = ''; lead = '' }
      await invalidateAll()
      saved = true
    } catch (e) { error = errorText(t, e); if (e instanceof ApiError) fields = e.fields }
    finally { busy = false }
  }
</script>
<form onsubmit={(e) => { e.preventDefault(); save() }} novalidate aria-busy={busy}>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if saved}<p role="status">{t.saved}</p>{/if}
  <label>{t.teamName}<input bind:value={name} maxlength="120" aria-invalid={!!fields.name} />
    {#if fields.name}<span class="error">{fieldText(t, fields.name[0])}</span>{/if}
  </label>
  <label>{t.teamLead}<select bind:value={lead} aria-invalid={!!fields.lead_account_id}>
    <option value="">{t.chooseLead}</option>
    {#each eligible as account}<option value={account.id}>{account.username} — {t.roles[account.role]} ({account.id})</option>{/each}
  </select>
    {#if fields.lead_account_id}<span class="error">{fieldText(t, fields.lead_account_id[0])}</span>{/if}
  </label>
  <p class="hint">{t.leadHint}</p>
  <div class="actions">
    <button type="submit" disabled={busy}>{busy ? t.saving : !team ? t.createTeam : team.archived ? t.restoreTeam : t.save}</button>
    {#if team && !team.archived}<button type="button" disabled={busy} onclick={() => save(true)}>{t.archiveTeam}</button>{/if}
  </div>
  {#if team && !team.archived}<p class="hint">{t.archiveHint}</p>{/if}
</form>
<style>form { max-width: 40rem; padding: 1rem 0; } p { margin: 0; }</style>
