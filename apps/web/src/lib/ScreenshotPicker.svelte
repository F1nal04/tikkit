<script lang="ts">
  // Screenshot selection for the ticket form: uploads each file, previews it,
  // and keeps `ids` (the intake's `screenshot_ids`) in sync. Failures are shown
  // per file and never touch the rest of the form.
  import { untrack } from 'svelte'
  import { api as apiClient } from '#lib/api.ts'
  import { messages, type Locale } from '#lib/i18n.ts'
  import { ACCEPT, precheck, uploadErrorText, uploadScreenshot, type Screenshot } from '#lib/screenshots.ts'

  let { locale, ids = $bindable([]), error }: { locale: Locale; ids?: string[]; error?: string } = $props()
  let t = $derived(messages[locale])
  let known = $state<Record<string, Screenshot>>({})
  let problems = $state<{ name: string; text: string }[]>([])
  let busy = $state(0)
  const inputId = `screenshots-${Math.random().toString(36).slice(2)}`

  // Resumed drafts only carry IDs; load their metadata (owner-only while pending).
  $effect(() => {
    const api = apiClient(locale)
    for (const id of ids) {
      if (untrack(() => known[id])) continue
      api
        .request('get', '/api/screenshots/{id}', { path: { id } })
        .then((s) => (known[id] = s))
        .catch(() => {})
    }
  })

  async function add(event: Event) {
    const input = event.currentTarget as HTMLInputElement
    const files = [...(input.files ?? [])]
    input.value = ''
    problems = []
    for (const file of files) {
      const early = precheck(t, file, ids.length + busy)
      if (early) {
        problems.push({ name: file.name, text: early })
        continue
      }
      busy++
      try {
        const screenshot = await uploadScreenshot(file)
        known[screenshot.id] = screenshot
        ids = [...ids, screenshot.id]
      } catch (error) {
        problems.push({ name: file.name, text: uploadErrorText(t, error) })
      } finally {
        busy--
      }
    }
  }

  // Dropping the reference is enough: unreferenced uploads expire server-side.
  const remove = (id: string) => (ids = ids.filter((other) => other !== id))
</script>

<fieldset class="screenshots">
  <legend>{t.screenshots.label}</legend>
  <p id="{inputId}-hint" class="hint">{t.screenshots.hint}</p>
  <label for={inputId}>{t.screenshots.add}</label>
  <input
    id={inputId}
    type="file"
    accept={ACCEPT.join(',')}
    multiple
    aria-invalid={error ? 'true' : undefined}
    aria-describedby={error ? `${inputId}-hint ${inputId}-error` : `${inputId}-hint`}
    onchange={add}
  />
  {#if error}<p id="{inputId}-error" class="error">{error}</p>{/if}
  {#if busy}<p aria-live="polite">{t.screenshots.uploading}</p>{/if}
  {#if problems.length}
    <ul role="alert">
      {#each problems as problem, i (i)}
        <li>{problem.name}: {problem.text}</li>
      {/each}
    </ul>
  {/if}
  {#if ids.length}
    <ul class="previews">
      {#each ids as id (id)}
        {@const name = known[id]?.filename ?? id}
        <li>
          <img src="/api/screenshots/{id}/preview" alt={name} loading="lazy" />
          <span>{name}</span>
          <button type="button" aria-label="{t.screenshots.remove}: {name}" onclick={() => remove(id)}>
            {t.screenshots.remove}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</fieldset>

<style>
  .previews {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem;
    padding: 0;
    list-style: none;
  }
  .previews li {
    display: grid;
    gap: 0.25rem;
    max-width: 10rem;
  }
  .previews img {
    max-width: 10rem;
    max-height: 7.5rem;
    object-fit: contain;
  }
  .previews span {
    overflow-wrap: anywhere;
  }
</style>
