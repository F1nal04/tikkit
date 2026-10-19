<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { ApiError, type AccountSummary, type Role, type Schemas } from '@tikkit/api-client'
  import { api as apiClient } from '#lib/api.ts'
  import Alert from '#lib/components/Alert.svelte'
  import Field from '#lib/components/Field.svelte'
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
  let error = $state<unknown>(null)
  let saved = $state(false)
  let fields = $state<Record<string, string[]>>({})

  const api = () => apiClient(locale)
  async function run(action: () => Promise<unknown>) {
    if (busy) return
    busy = true
    error = null
    saved = false
    fields = {}
    try {
      await action()
      await invalidateAll()
      saved = true
    } catch (e) {
      error = e
      if (e instanceof ApiError) fields = e.fields
    } finally {
      busy = false
    }
  }
  function save(event: SubmitEvent) {
    event.preventDefault()
    return run(() =>
      api().request('put', '/api/accounts/{id}', {
        path: { id: account.id },
        body: {
          role,
          expertise: role === 'specialist' ? expertise || null : null,
          team_id: role !== 'admin' && role !== 'user' ? team || null : null,
        },
      }),
    )
  }
  const setActive = () =>
    run(() =>
      api().request('post', account.is_active ? '/api/accounts/{id}/deactivate' : '/api/accounts/{id}/reactivate', {
        path: { id: account.id },
      }),
    )
</script>

<article aria-label={account.username}>
  <h3>{account.username} <small>{t.roles[account.role]} · {account.is_active ? t.active : t.inactive}</small></h3>
  <p>{account.email}<br /><small>{t.accountId}: {account.id}</small></p>
  <details>
    <summary>{t.editAccount}: {account.username}</summary>
    <form onsubmit={save} novalidate aria-busy={busy}>
      {#if error}<Alert>{errorText(t, error)}</Alert>{/if}
      {#if saved}<Alert kind="success">{t.saved}</Alert>{/if}
      <Field
        name="role-{account.id}"
        label={t.role}
        bind:value={role}
        options={roles.map((r) => ({ value: r, label: t.roles[r] }))}
      />
      {#if role === 'specialist'}
        <Field
          name="expertise-{account.id}"
          label={t.expertise}
          bind:value={expertise}
          hint={t.expertiseHint}
          error={fieldText(t, fields.expertise?.[0])}
        />
      {/if}
      {#if role !== 'admin' && role !== 'user'}
        <Field
          name="team-{account.id}"
          label={t.team}
          bind:value={team}
          options={[{ value: '', label: t.noTeam }, ...teams.filter((x) => !x.archived).map((x) => ({ value: x.id, label: x.name }))]}
          error={fieldText(t, fields.team_id?.[0])}
        />
      {/if}
      {#if account.is_team_lead}
        <p>{t.leadership}: {teams.find((x) => x.id === account.team_id)?.name} — <a href="/admin/teams">{t.teamsTitle}</a></p>
      {/if}
      <p class="hint">{t.leadProtection}</p>
      <div class="actions">
        <button type="submit" disabled={busy}>{busy ? t.saving : t.save}</button>
        <button type="button" class="secondary" disabled={busy} onclick={setActive}>
          {account.is_active ? t.deactivate : t.reactivate}
        </button>
      </div>
    </form>
  </details>
</article>

<style>
  article { padding: 1rem 0; border-top: 1px solid var(--line); }
  h3 { margin: 0.25rem 0; overflow-wrap: anywhere; }
  h3 small { display: block; font-size: 0.875rem; font-weight: 400; margin-top: 0.35rem; }
  p { overflow-wrap: anywhere; }
  form { padding-top: 1rem; max-width: 36rem; }
  summary { cursor: pointer; text-decoration: underline; text-underline-offset: 0.25em; }
</style>
