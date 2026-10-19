<script lang="ts">
  import { messages } from '#lib/i18n.ts'
  import TeamEditor from './TeamEditor.svelte'
  import type { PageProps } from './$types'

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
</script>

<svelte:head><title>{t.teamsTitle}</title></svelte:head>

<h1>{t.teamsTitle}</h1>
<p>{t.separateAccounts}</p>
<p class="hint">{t.leadProtection}</p>
<h2>{t.createTeam}</h2>
<TeamEditor accounts={data.accounts} locale={data.locale} />
{#each data.teams as team (team.id)}
  <section aria-label={team.name}>
    <h2>{team.name} {#if team.archived}<small>({t.archived})</small>{/if}</h2>
    {#key `${team.id}:${team.archived}:${team.lead_account_id}`}
      <TeamEditor {team} accounts={data.accounts} locale={data.locale} />
    {/key}
  </section>
{:else}<p>{t.noTeams}</p>{/each}
<p class="actions">
  <a href="/admin/teams">{t.firstPage}</a>
  {#if data.nextCursor}<a href={`?cursor=${encodeURIComponent(data.nextCursor)}`}>{t.nextPage}</a>{/if}
</p>

<style>
  section { border-top: 1px solid var(--line); margin-top: 1.5rem; }
</style>
