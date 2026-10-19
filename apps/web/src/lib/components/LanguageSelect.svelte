<script lang="ts">
  import { goto, invalidateAll } from '$app/navigation'
  import type { Account, Language } from '@tikkit/api-client'
  import { api } from '#lib/api.ts'
  import { messages } from '#lib/i18n.ts'

  let { locale, user }: { locale: Language; user: Account | null } = $props()
  let t = $derived(messages[locale])

  // Only the language data reloads; mounted pages keep their unsent input.
  async function change(event: Event & { currentTarget: HTMLSelectElement }) {
    const next = event.currentTarget.value as Language
    document.cookie = `lang=${next}; path=/; max-age=31536000; samesite=lax`
    // Signed in, the account's preference is what persists (and wins over the cookie).
    if (user) await api(locale).request('patch', '/api/auth/me', { body: { language: next } }).catch(() => undefined)
    // An explicit ?lang= would keep overriding the new choice.
    const url = new URL(location.href)
    if (url.searchParams.has('lang')) {
      url.searchParams.delete('lang')
      await goto(url, { replaceState: true, invalidateAll: true })
    } else await invalidateAll()
  }
</script>

<div class="field">
  <label for="ui-language">{t.language}</label>
  <select id="ui-language" value={locale} onchange={change}>
    <option value="en">{t.languages.en}</option>
    <option value="de">{t.languages.de}</option>
  </select>
</div>
