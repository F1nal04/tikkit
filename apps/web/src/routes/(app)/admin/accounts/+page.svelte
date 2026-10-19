<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { ApiError, createClient, type Language, type Role } from '@tikkit/api-client'
  import { errorText, fieldText, messages } from '#lib/i18n.ts'
  import { invitationTime } from '#lib/admin.ts'
  import AccountEditor from './AccountEditor.svelte'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  const roles: Role[] = ['user', 'operator', 'specialist', 'team_lead', 'admin']
  let role = $state<Role>('user')
  let email = $state('')
  // svelte-ignore state_referenced_locally
  let language = $state<Language>(data.organization?.default_language ?? data.locale)
  let hours = $state(168)
  let created = $state<{ url: string; emailed: boolean } | null>(null)
  let copied = $state(false)
  let busy = $state(false)
  let error = $state<string | null>(null)
  let fields = $state<Record<string, string[]>>({})

  const api = () => createClient({ language: data.locale })
  const when = (iso: string) => invitationTime(iso, data.locale, data.organization?.timezone ?? 'UTC')

  async function run(action: () => Promise<unknown>) {
    if (busy) return
    busy = true
    error = null
    fields = {}
    try {
      await action()
      await invalidateAll()
    } catch (e) {
      error = errorText(t, e)
      if (e instanceof ApiError) fields = e.fields
    } finally { busy = false }
  }

  const createInvitation = (event: SubmitEvent) => {
    event.preventDefault()
    return run(async () => {
      const r = await api().request('post', '/api/invitations', {
        body: { role, email: email || null, language, expires_in_hours: hours },
      })
      created = { url: r.url, emailed: r.emailed }
      copied = false
      email = ''
    })
  }

  const revoke = (id: string) =>
    run(() => api().request('post', '/api/invitations/{id}/revoke', { path: { id } }))
  const reissue = (id: string) => run(async () => {
    const result = await api().request('post', '/api/invitations/{id}/reissue', { path: { id } })
    created = { url: result.url, emailed: result.emailed }
    copied = false
  })

  async function copy() {
    if (!created) return
    try { await navigator.clipboard.writeText(created.url); copied = true }
    catch (e) { error = errorText(t, e) }
  }
</script>

<svelte:head><title>{t.accountsTitle}</title></svelte:head>

<main class="wide admin-page">
  <h1>{t.accountsTitle}</h1>
  <p class="hint">{t.separateAccounts}</p>
  {#if error}<p class="error" role="alert">{error}</p>{/if}

  <h2>{t.invite}</h2>
  <form onsubmit={createInvitation} novalidate aria-busy={busy}>
    <label>
      {t.role}
      <select name="role" bind:value={role}>
        {#each roles as r (r)}<option value={r}>{t.roles[r]}</option>{/each}
      </select>
    </label>
    <label>
      {t.inviteEmailOptional}
      <input name="email" type="email" bind:value={email} aria-invalid={!!fields.email} />
      {#if fields.email}<span class="error">{fieldText(t, fields.email[0])}</span>{/if}
    </label>
    <label>
      {t.language}
      <select name="language" bind:value={language}>
        <option value="en">{t.languages.en}</option>
        <option value="de">{t.languages.de}</option>
      </select>
    </label>
    <label>
      {t.inviteExpiresHours}
      <input name="expires_in_hours" type="number" min="1" max="720" bind:value={hours} aria-invalid={!!fields.expires_in_hours} />
      {#if fields.expires_in_hours}<span class="error">{fieldText(t, fields.expires_in_hours[0])}</span>{/if}
    </label>
    <button type="submit" disabled={busy}>{t.inviteCreate}</button>
  </form>
  {#if created}
    <div role="status">
      <label>
        {t.inviteLink}
        <input readonly value={created.url} onfocus={(e) => e.currentTarget.select()} />
      </label>
      <button type="button" onclick={copy}>{t.copy}</button>
      {#if copied}<span>{t.copied}</span>{/if}
      <p>{created.emailed ? t.inviteEmailed : t.inviteNotEmailed}</p>
    </div>
  {/if}

  <h2>{t.invitationHistory}</h2>
  <p class="hint">{t.reissueHint}</p>
  {#if data.invitations.length === 0}
    <p>{t.noInvitations}</p>
  {:else}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex (keyboard access to horizontally scrolling invitation history) -->
    <div class="table-scroll" role="region" aria-label={t.invitationHistory} tabindex="0"><table>
      <thead><tr><th>{t.username}</th><th>{t.role}</th><th>{t.email}</th><th>{t.status}</th><th>{t.expires}</th><th>{t.invite}</th></tr></thead>
      <tbody>
        {#each data.invitations as i (i.id)}
          <tr>
            <td>{i.username ?? '—'}{#if i.account_id}<br /><small>{i.account_id}</small>{/if}</td>
            <td>{t.roles[i.role]}</td>
            <td>{i.email ?? '—'}</td>
            <td>{t.invitationStatuses[i.status]}</td>
            <td>{when(i.expires_at)}</td>
            <td>{#if i.status === 'pending' || i.status === 'expired'}
              <button type="button" disabled={busy} onclick={() => revoke(i.id)}>{t.revoke}</button>
              <button type="button" disabled={busy} onclick={() => reissue(i.id)}>{t.reissue}</button>
            {/if}</td>
          </tr>
        {/each}
      </tbody>
    </table></div>
  {/if}
  <div class="actions">
    <a href="/admin/accounts">{t.firstPage}</a>
    {#if data.invitationCursor}<a href={`?invitations=${encodeURIComponent(data.invitationCursor)}`}>{t.nextPage}</a>{/if}
  </div>

  <h2>{t.accountsTitle}</h2>
  {#each data.accounts as account (account.id)}
    <AccountEditor {account} teams={data.teams} locale={data.locale} />
  {:else}<p>{t.noAccounts}</p>{/each}
  <div class="actions">
    <a href="/admin/accounts">{t.firstPage}</a>
    {#if data.accountCursor}<a href={`?accounts=${encodeURIComponent(data.accountCursor)}`}>{t.nextPage}</a>{/if}
  </div>
</main>
<style>
  .table-scroll { overflow-x: auto; }
  form { max-width: 36rem; }
  .actions { margin: 1rem 0; }
  td button { margin: .25rem; }
</style>
