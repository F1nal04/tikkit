<script lang="ts">
  type Option = { value: string; label: string }
  type Props = {
    label: string
    name: string
    value?: string
    type?: string
    hint?: string
    /** Already translated, e.g. `fieldText(t, fields.name?.[0])`. */
    error?: string
    autocomplete?: AutoFill
    required?: boolean
    /** Renders a select instead of an input. */
    options?: Option[]
    /** Renders a textarea instead of an input. */
    multiline?: boolean
  }
  // Binding `value` keeps what the user typed when a request fails and the form re-renders.
  let { label, name, value = $bindable(''), type = 'text', hint, error, autocomplete, required, options, multiline }: Props = $props()
  let describedBy = $derived([hint && `${name}-hint`, error && `${name}-error`].filter(Boolean).join(' ') || undefined)
</script>

<div class="field">
  <label for={name}>{label}</label>
  {#if options}
    <select id={name} {name} {required} bind:value aria-invalid={!!error} aria-describedby={describedBy}>
      {#each options as option (option.value)}<option value={option.value}>{option.label}</option>{/each}
    </select>
  {:else if multiline}
    <textarea id={name} {name} {required} rows="6" bind:value aria-invalid={!!error} aria-describedby={describedBy}></textarea>
  {:else}
    <input
      id={name}
      {name}
      {type}
      {autocomplete}
      {required}
      bind:value
      aria-invalid={!!error}
      aria-describedby={describedBy}
    />
  {/if}
  {#if hint}<span id="{name}-hint" class="hint">{hint}</span>{/if}
  {#if error}<span id="{name}-error" class="error">{error}</span>{/if}
</div>
