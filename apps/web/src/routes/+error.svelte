<script lang="ts">
  import { page } from '$app/state'
  import ErrorView from '#lib/components/ErrorView.svelte'

  // Rendered inside the root layout, so signed-in users keep the shell around it.
  // `page.data` has no locale only if the root layout itself failed to load.
  const locale = $derived(page.data.locale ?? 'en')
</script>

<ErrorView status={page.status} {locale} />
