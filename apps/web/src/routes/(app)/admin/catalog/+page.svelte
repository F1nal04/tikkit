<script lang="ts">
  import { invalidateAll } from '$app/navigation'
  import { ApiError, type Schemas } from '@tikkit/api-client'
  import { api as apiClient } from '#lib/api.ts'
  import Alert from '#lib/components/Alert.svelte'
  import Field from '#lib/components/Field.svelte'
  import { errorText, fieldText, messages } from '#lib/i18n.ts'
  import { fieldBody } from './form.ts'
  import type { PageProps } from './$types'

  type IntakeField = Schemas['IntakeField']

  let { data }: PageProps = $props()
  let t = $derived(messages[data.locale])
  let catalog = $derived(data.catalog)
  let newKind = $state<string>('text')
  // The failed form's key and message, shown next to that form.
  let problem = $state<{ at: string; text: string } | null>(null)

  const api = () => apiClient(data.locale)

  async function run(at: string, action: () => Promise<unknown>): Promise<boolean> {
    problem = null
    try {
      await action()
      await invalidateAll()
      return true
    } catch (e) {
      const codes = e instanceof ApiError ? Object.values(e.fields).flat() : []
      const text = codes.length
        ? codes.map((code) => (code === 'taken' ? t.catalog.taken : fieldText(t, code))).join(' ')
        : errorText(t, e)
      problem = { at, text }
      return false
    }
  }

  /** Submit handler; `fresh` forms (creation) are cleared after success. */
  const submit =
    (at: string, send: (form: FormData) => Promise<unknown>, fresh = false) =>
    async (event: SubmitEvent) => {
      event.preventDefault()
      const form = event.currentTarget as HTMLFormElement
      if ((await run(at, () => send(new FormData(form)))) && fresh) {
        form.reset()
        newKind = 'text'
      }
    }

  const order = {
    categories: (ids: string[]) => api().request('put', '/api/categories/order', { body: { ids } }),
    priorities: (ids: string[]) => api().request('put', '/api/priorities/order', { body: { ids } }),
    fields: (ids: string[]) => api().request('put', '/api/intake-fields/order', { body: { ids } }),
  }
  const archive = {
    categories: (id: string, archived: boolean) =>
      api().request('patch', '/api/categories/{id}', { path: { id }, body: { archived } }),
    priorities: (id: string, archived: boolean) =>
      api().request('patch', '/api/priorities/{id}', { path: { id }, body: { archived } }),
    fields: (id: string, archived: boolean) =>
      api().request('patch', '/api/intake-fields/{id}', { path: { id }, body: { archived } }),
  }
  type Kind = keyof typeof order

  function move(kind: Kind, items: { id: string }[], index: number, by: -1 | 1) {
    const ids = items.map((i) => i.id)
    ;[ids[index], ids[index + by]] = [ids[index + by], ids[index]]
    return run(kind, () => order[kind](ids))
  }

  const named = (f: FormData) => String(f.get('name') ?? '')
</script>

<svelte:head><title>{t.catalog.title}</title></svelte:head>

{#snippet problemAt(at: string)}
  {#if problem?.at === at}<Alert>{problem.text}</Alert>{/if}
{/snippet}

{#snippet controls(kind: Kind, items: { id: string; archived: boolean }[], index: number, name: string)}
  <button type="button" aria-label="{t.catalog.moveUp}: {name}" disabled={index === 0} onclick={() => move(kind, items, index, -1)}>↑</button>
  <button
    type="button"
    aria-label="{t.catalog.moveDown}: {name}"
    disabled={index === items.length - 1}
    onclick={() => move(kind, items, index, 1)}>↓</button
  >
  <button type="button" onclick={() => run(kind, () => archive[kind](items[index].id, !items[index].archived))}>
    {items[index].archived ? t.catalog.restore : t.catalog.archive}
  </button>
{/snippet}

{#snippet fieldInputs(field: IntakeField)}
  <label>{t.catalog.label} <input name="label" value={field.label} required maxlength="100" /></label>
  <label>{t.catalog.guidance} <textarea name="guidance" maxlength="1000" value={field.guidance}></textarea></label>
  <label class="check"><input type="checkbox" name="required" checked={field.required} /> {t.catalog.required}</label>
  <fieldset>
    <legend>{t.catalog.showFor}</legend>
    {#each catalog.categories as c (c.id)}
      <label class="check">
        <input type="checkbox" name="category_ids" value={c.id} checked={field.category_ids.includes(c.id)} />
        {c.name}{c.archived ? ` (${t.catalog.archived})` : ''}
      </label>
    {/each}
  </fieldset>
  {#if field.kind === 'select'}
    <label class="check"><input type="checkbox" name="allow_other" checked={field.allow_other} /> {t.catalog.allowOther}</label>
    <fieldset>
      <legend>{t.catalog.choices}</legend>
      {#each field.choices as choice (choice.id)}
        <div class="row">
          <input type="hidden" name="choice_id" value={choice.id} />
          <input name="choice_label" value={choice.label} aria-label={t.catalog.label} required maxlength="100" />
          <label class="check">
            <input type="checkbox" name="choice_archived" value={choice.id} checked={choice.archived} />
            {t.catalog.archived}
          </label>
        </div>
      {/each}
      <label>{t.catalog.newChoices} <textarea name="new_choices"></textarea></label>
    </fieldset>
  {/if}
{/snippet}

<h1>{t.catalog.title}</h1>
<p>{t.catalog.intro}</p>
<p class="hint">{t.catalog.archivedHint}</p>

<h2>{t.catalog.categories}</h2>
{@render problemAt('categories')}
<ul>
  {#each catalog.categories as c, i (c.id)}
    <li class="row">
      <form
        class="row"
        onsubmit={submit('categories', (f) =>
          api().request('patch', '/api/categories/{id}', {
            path: { id: c.id },
            body: { name: named(f), skip_ai: f.has('skip_ai') },
          }),
        )}
      >
        <label>{t.catalog.name} <input name="name" value={c.name} required maxlength="100" /></label>
        <label class="check"><input type="checkbox" name="skip_ai" checked={c.skip_ai} /> {t.catalog.skipAi}</label>
        {#if c.archived}<span>{t.catalog.archived}</span>{/if}
        <button type="submit" aria-label="{t.save}: {c.name}">{t.save}</button>
      </form>
      {@render controls('categories', catalog.categories, i, c.name)}
    </li>
  {:else}
    <li>{t.catalog.none}</li>
  {/each}
</ul>
<form
  onsubmit={submit(
    'category-new',
    (f) => api().request('post', '/api/categories', { body: { name: named(f), skip_ai: f.has('skip_ai') } }),
    true,
  )}
>
  <label>{t.catalog.name} <input name="name" required maxlength="100" /></label>
  <label class="check">
    <input type="checkbox" name="skip_ai" aria-describedby="skip-ai-hint" />
    {t.catalog.skipAi}
  </label>
  <p class="hint" id="skip-ai-hint">{t.catalog.skipAiHint}</p>
  {@render problemAt('category-new')}
  <button type="submit">{t.catalog.add}</button>
</form>

<h2>{t.catalog.priorities}</h2>
<p class="hint">{t.catalog.prioritiesHint}</p>
{@render problemAt('priorities')}
<ul>
  {#each catalog.priorities as p, i (p.id)}
    <li class="row">
      <form
        class="row"
        onsubmit={submit('priorities', (f) =>
          api().request('patch', '/api/priorities/{id}', { path: { id: p.id }, body: { name: named(f) } }),
        )}
      >
        <label>{t.catalog.name} <input name="name" value={p.name} required maxlength="100" /></label>
        {#if p.archived}<span>{t.catalog.archived}</span>{/if}
        <button type="submit" aria-label="{t.save}: {p.name}">{t.save}</button>
      </form>
      {@render controls('priorities', catalog.priorities, i, p.name)}
    </li>
  {:else}
    <li>{t.catalog.none}</li>
  {/each}
</ul>
<form
  onsubmit={submit('priority-new', (f) => api().request('post', '/api/priorities', { body: { name: named(f) } }), true)}
>
  <label>{t.catalog.name} <input name="name" required maxlength="100" /></label>
  {@render problemAt('priority-new')}
  <button type="submit">{t.catalog.add}</button>
</form>

<h2>{t.catalog.fields}</h2>
{@render problemAt('fields')}
<ul>
  {#each catalog.fields as field, i (field.id)}
    <li>
      <details>
        <summary>
          {field.label} ({t.catalog.kinds[field.kind]}){field.archived ? ` — ${t.catalog.archived}` : ''}
        </summary>
        <form
          onsubmit={submit('fields', (f) =>
            api().request('patch', '/api/intake-fields/{id}', { path: { id: field.id }, body: fieldBody(f) }),
          )}
        >
          {@render fieldInputs(field)}
          <button type="submit">{t.save}</button>
        </form>
      </details>
      {@render controls('fields', catalog.fields, i, field.label)}
    </li>
  {:else}
    <li>{t.catalog.none}</li>
  {/each}
</ul>
<h3>{t.catalog.addField}</h3>
<form
  onsubmit={submit(
    'field-new',
    (f) => api().request('post', '/api/intake-fields', { body: { ...fieldBody(f), kind: newKind as IntakeField['kind'] } }),
    true,
  )}
>
  <Field
    name="kind"
    label={t.catalog.kind}
    bind:value={newKind}
    options={[{ value: 'text', label: t.catalog.kinds.text }, { value: 'select', label: t.catalog.kinds.select }]}
  />
  {@render fieldInputs({
    id: '',
    label: '',
    guidance: '',
    kind: newKind as IntakeField['kind'],
    required: false,
    allow_other: false,
    category_ids: [],
    choices: [],
    archived: false,
  })}
  {@render problemAt('field-new')}
  <button type="submit">{t.catalog.addField}</button>
</form>

<style>
  ul { padding: 0; list-style: none; display: grid; gap: 0.75rem; }
  .row { display: flex; flex-wrap: wrap; gap: 0.5rem; align-items: end; }
  .check { display: flex; gap: 0.5rem; align-items: center; font-weight: 400; }
  fieldset { display: grid; gap: 0.5rem; border: 1px solid #cad3cb; border-radius: 0.375rem; }
  button:disabled { opacity: 0.4; cursor: default; }
</style>
