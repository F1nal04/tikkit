<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import type { Notification } from '@tikkit/api-client'
  import Alert from '#lib/components/Alert.svelte'
  import EmptyState from '#lib/components/EmptyState.svelte'
  import { api as apiClient } from '#lib/api.ts'
  import { errorText, messages } from '#lib/i18n.ts'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let n = $derived(t.notifications)
  // The loaded page is the starting point; "load more" appends to it and a reload replaces it.
  let loaded = $state<Notification[]>([])
  let cursor = $state<string | null>(null)
  let error = $state<unknown>(null)
  $effect(() => {
    loaded = data.page?.items ?? []
    cursor = data.page?.next_cursor ?? null
  })
  const api = () => apiClient(data.locale)
  const when = (iso: string) => new Date(iso).toLocaleString(data.locale, { timeZone: 'UTC', timeZoneName: 'short' })

  async function more() {
    error = null
    try {
      const page = await api().request('get', '/api/notifications', { query: { limit: 25, cursor: cursor ?? undefined } })
      loaded = [...loaded, ...page.items]
      cursor = page.next_cursor
    } catch (e) {
      error = e
    }
  }

  async function markRead(id: string) {
    error = null
    try {
      await api().request('post', '/api/notifications/{id}/read', { path: { id } })
      loaded = loaded.map((item) => (item.id === id ? { ...item, read_at: new Date().toISOString() } : item))
      // Refreshes the unread count in the shell.
      await invalidateAll()
    } catch (e) {
      error = e
    }
  }
</script>

<svelte:head><title>{n.title}</title></svelte:head>
<h1>{n.title}</h1>
{#if data.unavailable}
  <Alert>{errorText(t, { code: 'unavailable' })}</Alert>
{:else if loaded.length === 0}
  <EmptyState title={n.empty}>{n.emptyHint}</EmptyState>
{:else}
  <ul class="notifications">
    {#each loaded as item (item.id)}
      <li class:unread={!item.read_at}>
        <p><strong>{item.title}</strong></p>
        <p>{item.body}</p>
        <p class="meta">
          <time datetime={item.created_at}>{when(item.created_at)}</time>
          · <a href={item.href}>{n.open}</a>
          {#if !item.read_at}
            · <button type="button" class="secondary" onclick={() => markRead(item.id)}>{n.markRead}</button>
          {/if}
        </p>
      </li>
    {/each}
  </ul>
  {#if cursor}<button type="button" onclick={more}>{n.more}</button>{/if}
{/if}
{#if error}<Alert>{errorText(t, error)}</Alert>{/if}
