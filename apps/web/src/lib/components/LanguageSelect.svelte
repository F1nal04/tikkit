<script lang="ts">
  import { goto, invalidateAll } from '$app/navigation'
  import type { Account, Language } from '@tikkit/api-client'
  import { api } from '#lib/api.ts'
  import { errorText, messages } from '#lib/i18n.ts'
  import Alert from './Alert.svelte'

  let { locale, user }: { locale: Language; user: Account | null } = $props()
  let t = $derived(messages[locale])
  let error = $state<unknown>(null)
  let busy = $state(false)

  // Only the language data reloads; mounted pages keep their unsent input.
  async function change(event: Event & { currentTarget: HTMLSelectElement }) {
    const next = event.currentTarget.value as Language
    const select = event.currentTarget
    busy = true
    error = null
    try {
      // Do not reload on a failed save: an expired session would redirect away from unsent input.
      if (user) await api(locale).request('patch', '/api/auth/me', { body: { language: next } })
      document.cookie = `lang=${next}; path=/; max-age=31536000; samesite=lax`
      // An explicit ?lang= would keep overriding the new choice.
      const url = new URL(location.href)
      if (url.searchParams.has('lang')) {
        url.searchParams.delete('lang')
        await goto(url, { replaceState: true, invalidateAll: true })
      } else await invalidateAll()
    } catch (e) {
      error = e
      select.value = locale
    } finally {
      busy = false
    }
  }
</script>

<div class="field">
  <label for="ui-language">{t.language}</label>
  <select id="ui-language" value={locale} onchange={change} disabled={busy}>
    <option value="en">{t.languages.en}</option>
    <option value="de">{t.languages.de}</option>
  </select>
</div>
{#if error}<Alert>{errorText(t, error)}</Alert>{/if}
