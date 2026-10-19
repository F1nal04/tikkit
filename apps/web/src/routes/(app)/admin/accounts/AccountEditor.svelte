<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { ApiError, createClient, type AccountSummary, type Role, type Schemas } from '@tikkit/api-client'
  import { errorText, fieldText, messages, type Locale } from '#lib/i18n.ts'
  let { account, teams, locale }: { account: AccountSummary; teams: Schemas['Team'][]; locale: Locale } = $props()
  let t = $derived(messages[locale])
  const roles: Role[] = ['user', 'operator', 'specialist', 'team_lead', 'admin']
  // svelte-ignore state_referenced_locally
  let role = $state<Role>(account.role)
  // svelte-ignore state_referenced_locally
  let expertise = $state(account.expertise ?? '')
  // svelte-ignore state_referenced_locally
  let team = $state(account.team_id ?? '')
  let busy = $state(false)
  let error = $state<string | null>(null)
  let saved = $state(false)
  let fields = $state<Record<string, string[]>>({})
  async function run(action: () => Promise<unknown>) {
    if (busy) return
    busy = true; error = null; saved = false; fields = {}
    try { await action(); await invalidateAll(); saved = true }
    catch (e) { error = errorText(t, e); if (e instanceof ApiError) fields = e.fields }
    finally { busy = false }
  }
  function save(event: SubmitEvent) {
    event.preventDefault()
    return run(() => createClient().request('put', '/api/accounts/{id}', { path: { id: account.id }, body: { role, expertise: role === 'specialist' ? expertise || null : null, team_id: role !== 'admin' && role !== 'user' ? team || null : null } }))
  }
</script>
<article aria-label={account.username}>
  <h3>{account.username} <small>{t.roles[account.role]} · {account.is_active ? t.active : t.inactive}</small></h3>
  <p>{account.email}<br /><small>{t.accountId}: {account.id}</small></p>
  <details>
    <summary>{t.editAccount}: {account.username}</summary>
    <form onsubmit={save} novalidate aria-busy={busy}>
      {#if error}<p class="error" role="alert">{error}</p>{/if}
      {#if saved}<p role="status">{t.saved}</p>{/if}
      <label>{t.role}<select bind:value={role}>{#each roles as r}<option value={r}>{t.roles[r]}</option>{/each}</select></label>
      {#if role === 'specialist'}
        <label>{t.expertise}<input bind:value={expertise} maxlength="120" aria-invalid={!!fields.expertise} />
          {#if fields.expertise}<span class="error">{fieldText(t, fields.expertise[0])}</span>{/if}
        </label>
        <p class="hint">{t.expertiseHint}</p>
      {/if}
      {#if role !== 'admin' && role !== 'user'}
        <label>{t.team}<select bind:value={team} aria-invalid={!!fields.team_id}>
          <option value="">{t.noTeam}</option>
          {#each teams.filter((t) => !t.archived) as team}<option value={team.id}>{team.name}</option>{/each}
        </select>
        {#if fields.team_id}<span class="error">{fieldText(t, fields.team_id[0])}</span>{/if}</label>
      {/if}
      {#if account.is_team_lead}<p>{t.leadership}: {teams.find((t) => t.id === account.team_id)?.name} — <a href="/admin/teams">{t.teamsTitle}</a></p>{/if}
      <p class="hint">{t.leadProtection}</p>
      <div class="actions">
        <button type="submit" disabled={busy}>{busy ? t.saving : t.save}</button>
        <button type="button" disabled={busy} onclick={() => run(() => account.is_active
          ? createClient().request('post', '/api/accounts/{id}/deactivate', { path: { id: account.id } })
          : createClient().request('post', '/api/accounts/{id}/reactivate', { path: { id: account.id } }))}>{account.is_active ? t.deactivate : t.reactivate}</button>
      </div>
    </form>
  </details>
</article>
<style>
  article { padding: 1rem 0; border-top: 1px solid #cad3cb; }
  h3 { margin: .25rem 0; overflow-wrap: anywhere; } h3 small { display: block; font-size: .875rem; font-weight: 400; margin-top: .35rem; }
  p { overflow-wrap: anywhere; } form { padding-top: 1rem; max-width: 36rem; } summary { cursor: pointer; text-decoration: underline; text-underline-offset: .25em; }
</style>
