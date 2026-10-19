<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { ApiError, type Language, type Role } from '@tikkit/api-client'
  import { api as apiClient } from '#lib/api.ts'
  import Table from '#lib/components/Table.svelte'
  import { errorText, fieldText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  const roles: Role[] = ['user', 'operator', 'specialist', 'team_lead', 'admin']
  let role = $state<Role>('user')
  let email = $state('')
  // svelte-ignore state_referenced_locally
  let language = $state<Language>(data.locale)
  let hours = $state(168)
  let created = $state<{ url: string; emailed: boolean } | null>(null)
  let copied = $state(false)
  let error = $state<unknown>(null)
  let fields = $state<Record<string, string[]>>({})

  const api = () => apiClient(data.locale)
  const when = (iso: string) =>
    new Intl.DateTimeFormat(data.locale, { dateStyle: 'medium', timeStyle: 'short', timeZone: 'UTC' }).format(
      new Date(iso),
    ) + ' UTC'

  async function run(action: () => Promise<unknown>) {
    error = null
    fields = {}
    try {
      await action()
      await invalidateAll()
    } catch (e) {
      error = e
      if (e instanceof ApiError) fields = e.fields
    }
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
  const setActive = (id: string, active: boolean) =>
    run(() =>
      active
        ? api().request('post', '/api/accounts/{id}/reactivate', { path: { id } })
        : api().request('post', '/api/accounts/{id}/deactivate', { path: { id } }),
    )

  async function copy() {
    if (!created) return
    await navigator.clipboard.writeText(created.url)
    copied = true
  }
</script>

<svelte:head><title>{t.accountsTitle}</title></svelte:head>

  <h1>{t.accountsTitle}</h1>
  {#if error}<p class="error" role="alert">{errorText(t, error)}</p>{/if}

  <h2>{t.invite}</h2>
  <form onsubmit={createInvitation} novalidate>
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
    <button type="submit">{t.inviteCreate}</button>
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

  <h2>{t.pendingInvitations}</h2>
  <Table caption={t.pendingInvitations} empty={t.noPendingInvitations} isEmpty={data.invitations.length === 0}>
    {#snippet head()}
      <tr>
        <th scope="col">{t.role}</th><th scope="col">{t.email}</th><th scope="col">{t.expires}</th>
        <th scope="col"><span class="sr-only">{t.actions}</span></th>
      </tr>
    {/snippet}
    {#each data.invitations as i (i.id)}
      <tr>
        <td>{t.roles[i.role]}</td>
        <td>{i.email ?? '—'}</td>
        <td>{when(i.expires_at)}</td>
        <td><button type="button" onclick={() => revoke(i.id)}>{t.revoke}</button></td>
      </tr>
    {/each}
  </Table>

  <h2>{t.accountsTitle}</h2>
  <Table caption={t.accountsTitle} empty={t.noAccounts} isEmpty={data.accounts.length === 0}>
    {#snippet head()}
      <tr>
        <th scope="col">{t.username}</th><th scope="col">{t.email}</th><th scope="col">{t.role}</th>
        <th scope="col">{t.language}</th><th scope="col">{t.status}</th>
        <th scope="col"><span class="sr-only">{t.actions}</span></th>
      </tr>
    {/snippet}
    {#each data.accounts as a (a.id)}
      <tr>
        <td>{a.username}</td>
        <td>{a.email}</td>
        <td>{t.roles[a.role]}</td>
        <td>{t.languages[a.language]}</td>
        <td>{a.is_active ? t.active : t.inactive}</td>
        <td>
          <button type="button" onclick={() => setActive(a.id, !a.is_active)}>
            {a.is_active ? t.deactivate : t.reactivate}
          </button>
        </td>
      </tr>
    {/each}
  </Table>
