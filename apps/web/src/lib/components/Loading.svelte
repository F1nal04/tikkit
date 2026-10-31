<script lang="ts">
  let { label }: { label: string } = $props()
</script>

<p class="loading" role="status"><span class="spinner" aria-hidden="true"></span>{label}</p>
