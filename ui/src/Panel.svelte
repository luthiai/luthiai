<script>
  let { id, title, label, headingLevel = 3, closeLabel, onClose, closeDisabled = false, children, actions } = $props();
</script>

<section {id} class="panel" aria-label={label}>
  <header class="panel-heading">
    <svelte:element this={`h${headingLevel}`}>{title}</svelte:element>
    <!-- Material Web supplies keyboard activation inside its shadow DOM. -->
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <md-text-button disabled={closeDisabled} onclick={onClose}>{closeLabel}</md-text-button>
  </header>
  {@render children()}
  {#if actions}
    <div class="panel-actions">{@render actions()}</div>
  {/if}
</section>
