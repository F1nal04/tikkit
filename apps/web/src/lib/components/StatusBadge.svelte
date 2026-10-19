<script lang="ts">
  import type { TicketStatus } from '#lib/status.ts'

  let { status, label }: { status: TicketStatus; label: string } = $props()
</script>

<!-- The text is the indicator; colour only reinforces it. `data-status` is the stable identifier. -->
<span class="badge" data-status={status}>{label}</span>
