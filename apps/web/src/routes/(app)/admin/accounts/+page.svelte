<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { ApiError, type Language, type Role } from '@tikkit/api-client'
  import { api as apiClient } from '#lib/api.ts'
  import Alert from '#lib/components/Alert.svelte'
  import Field from '#lib/components/Field.svelte'
  import Table from '#lib/components/Table.svelte'
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
  let hours = $state('168')
  let created = $state<{ url: string; emailed: boolean } | null>(null)
  let copied = $state(false)
  let busy = $state(false)
  let error = $state<unknown>(null)
  let fields = $state<Record<string, string[]>>({})

  const api = () => apiClient(data.locale)
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
      error = e
      if (e instanceof ApiError) fields = e.fields
    } finally {
      busy = false
    }
  }

  const createInvitation = (event: SubmitEvent) => {
    event.preventDefault()
    return run(async () => {
      const r = await api().request('post', '/api/invitations', {
        body: { role, email: email || null, language, expires_in_hours: Number(hours) },
      })
      created = { url: r.url, emailed: r.emailed }
      copied = false
      email = ''
    })
  }

  const revoke = (id: string) =>
    run(() => api().request('post', '/api/invitations/{id}/revoke', { path: { id } }))
  const reissue = (id: string) =>
    run(async () => {
      const r = await api().request('post', '/api/invitations/{id}/reissue', { path: { id } })
      created = { url: r.url, emailed: r.emailed }
      copied = false
    })

  async function copy() {
    if (!created) return
    try {
      await navigator.clipboard.writeText(created.url)
      copied = true
    } catch (e) {
      error = e
    }
  }
</script>

<svelte:head><title>{t.accountsTitle}</title></svelte:head>

<h1>{t.accountsTitle}</h1>
<p class="hint">{t.separateAccounts}</p>
{#if error}<Alert>{errorText(t, error)}</Alert>{/if}

<h2>{t.invite}</h2>
<form onsubmit={createInvitation} novalidate aria-busy={busy}>
  <Field name="role" label={t.role} bind:value={role} options={roles.map((r) => ({ value: r, label: t.roles[r] }))} />
  <Field name="email" type="email" label={t.inviteEmailOptional} bind:value={email} error={fieldText(t, fields.email?.[0])} />
  <Field
    name="language"
    label={t.language}
    bind:value={language}
    options={[{ value: 'en', label: t.languages.en }, { value: 'de', label: t.languages.de }]}
  />
  <Field
    name="expires_in_hours"
    type="number"
    label={t.inviteExpiresHours}
    bind:value={hours}
    error={fieldText(t, fields.expires_in_hours?.[0])}
  />
  <div class="actions"><button type="submit" disabled={busy}>{t.inviteCreate}</button></div>
</form>
{#if created}
  <Alert kind="success">
    <label>
      {t.inviteLink}
      <input readonly value={created.url} onfocus={(e) => e.currentTarget.select()} />
    </label>
    <button type="button" onclick={copy}>{t.copy}</button>
    {#if copied}<span>{t.copied}</span>{/if}
    <p>{created.emailed ? t.inviteEmailed : t.inviteNotEmailed}</p>
  </Alert>
{/if}

<h2>{t.invitationHistory}</h2>
<p class="hint">{t.reissueHint}</p>
<Table caption={t.invitationHistory} empty={t.noInvitations} isEmpty={data.invitations.length === 0}>
  {#snippet head()}
    <tr>
      <th scope="col">{t.username}</th><th scope="col">{t.role}</th><th scope="col">{t.email}</th>
      <th scope="col">{t.status}</th><th scope="col">{t.expires}</th>
      <th scope="col"><span class="sr-only">{t.actions}</span></th>
    </tr>
  {/snippet}
  {#each data.invitations as i (i.id)}
    <tr>
      <td>{i.username ?? '—'}{#if i.account_id}<br /><small>{i.account_id}</small>{/if}</td>
      <td>{t.roles[i.role]}</td>
      <td>{i.email ?? '—'}</td>
      <td>{t.invitationStatuses[i.status]}</td>
      <td>{when(i.expires_at)}</td>
      <td>
        {#if i.status === 'pending' || i.status === 'expired'}
          <div class="actions">
            <button type="button" class="secondary" disabled={busy} onclick={() => revoke(i.id)}>{t.revoke}</button>
            <button type="button" class="secondary" disabled={busy} onclick={() => reissue(i.id)}>{t.reissue}</button>
          </div>
        {/if}
      </td>
    </tr>
  {/each}
</Table>
<p class="actions">
  <a href="/admin/accounts">{t.firstPage}</a>
  {#if data.invitationCursor}<a href={`?invitations=${encodeURIComponent(data.invitationCursor)}`}>{t.nextPage}</a>{/if}
</p>

<h2>{t.accountsTitle}</h2>
{#each data.accounts as account (account.id)}
  <AccountEditor {account} teams={data.teams} locale={data.locale} />
{:else}<p>{t.noAccounts}</p>{/each}
<p class="actions">
  <a href="/admin/accounts">{t.firstPage}</a>
  {#if data.accountCursor}<a href={`?accounts=${encodeURIComponent(data.accountCursor)}`}>{t.nextPage}</a>{/if}
</p>
