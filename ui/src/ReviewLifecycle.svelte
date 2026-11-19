<script>
  import { reviewLifecycleSteps } from './request-status.js';
  import TimeAgo from './TimeAgo.svelte';

  let { situation, request } = $props();
  const steps = $derived(reviewLifecycleSteps(situation, request));
  const stateLabels = { done: 'Completed: ', pending: 'Upcoming: ', attention: 'Needs attention: ', unavailable: 'Unavailable: ' };
</script>

<ol class="review-lifecycle" aria-label="Review lifecycle">
  {#each steps as step (step.id)}
    <li data-state={step.state} aria-current={['current', 'attention'].includes(step.state) ? 'step' : undefined}>
      <span class="stage-marker" aria-hidden="true">{step.state === 'done' ? '✓' : step.state === 'attention' ? '!' : ''}</span>
      <span class="stage-content">
        <span class="stage-label"><span class="visually-hidden">{stateLabels[step.state] ?? ''}</span>{step.label}</span>
        {#if step.detail || step.timestamp != null}
          <span class="stage-detail">
            {step.detail ?? ''}{#if step.detail && step.timestamp != null}{' · '}{/if}{#if step.timestamp != null}<TimeAgo value={step.timestamp} />{/if}
          </span>
        {/if}
      </span>
    </li>
  {/each}
</ol>

<style>
  .review-lifecycle {
    display: grid;
    grid-template-columns: repeat(5, minmax(0, 1fr));
    flex-basis: 100%;
    margin: 8px 0 12px;
    padding: 0;
    list-style: none;
  }

  li {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    min-width: 0;
    color: #656370;
  }

  li:not(:last-child)::after {
    content: '';
    position: absolute;
    top: 13px;
    left: calc(50% + 22px);
    width: calc(100% - 44px);
    height: 2px;
    background: #dedee7;
  }

  li[data-state='done']::after {
    background: #a7c9b0;
  }

  .stage-marker {
    display: grid;
    place-items: center;
    flex: none;
    width: 28px;
    height: 28px;
    border: 1px solid #c7c8d3;
    border-radius: 50%;
    background: #fff;
    font-size: 1rem;
    font-weight: 700;
  }

  .stage-content {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
    max-width: 100%;
    padding: 0 6px;
    text-align: center;
    overflow-wrap: anywhere;
  }

  .stage-label {
    font-size: 0.83rem;
    line-height: 1.45;
  }

  .stage-detail {
    color: #5b5963;
    font-size: 0.75rem;
    font-weight: 400;
    line-height: 1.45;
  }

  li[data-state='done'] {
    color: #245b30;
  }

  li[data-state='done'] .stage-marker {
    border-color: #dcefe0;
    background: #dcefe0;
  }

  li[data-state='current'] {
    color: var(--md-sys-color-primary);
    font-weight: 600;
  }

  li[data-state='current'] .stage-marker {
    border-color: var(--md-sys-color-primary);
    background: var(--md-sys-color-primary);
    box-shadow: 0 0 0 5px #edf0fb;
  }

  li[data-state='attention'] {
    color: #71510d;
    font-weight: 600;
  }

  li[data-state='attention'] .stage-marker {
    border-color: #71510d;
    background: #fff0d6;
    box-shadow: 0 0 0 5px #fff8eb;
  }

  li[data-state='unavailable'] {
    color: #76737e;
  }

  li[data-state='unavailable'] .stage-marker {
    border-style: dashed;
    border-color: #9c9aa6;
    background: #f4f3f7;
  }

  @media (max-width: 600px) {
    .review-lifecycle {
      grid-template-columns: minmax(0, 1fr);
      gap: 16px;
    }

    li {
      flex-direction: row;
      align-items: flex-start;
      gap: 12px;
    }

    .stage-content {
      padding: 4px 0 0;
      text-align: left;
    }

    li:not(:last-child)::after {
      top: 33px;
      left: 13px;
      width: 2px;
      height: calc(100% - 22px);
    }
  }
</style>
