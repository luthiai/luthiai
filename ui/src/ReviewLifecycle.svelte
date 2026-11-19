<script>
  import { reviewLifecycleSteps } from './request-status.js';

  let { situation } = $props();
  const steps = $derived(reviewLifecycleSteps(situation));
  const stateLabels = { done: 'Completed: ', pending: 'Upcoming: ', attention: 'Needs attention: ' };
</script>

<ol class="review-lifecycle" aria-label="Review lifecycle">
  {#each steps as step (step.id)}
    <li data-state={step.state} aria-current={['current', 'attention'].includes(step.state) ? 'step' : undefined}>
      <span class="stage-marker" aria-hidden="true">{step.state === 'done' ? '✓' : step.state === 'attention' ? '!' : ''}</span>
      <span class="stage-label"><span class="visually-hidden">{stateLabels[step.state] ?? ''}</span>{step.label}</span>
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

  .stage-label {
    padding: 0 6px;
    font-size: 0.83rem;
    line-height: 1.45;
    text-align: center;
    overflow-wrap: anywhere;
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

  @media (max-width: 600px) {
    .review-lifecycle {
      grid-template-columns: minmax(0, 1fr);
      gap: 16px;
    }

    li {
      flex-direction: row;
      gap: 12px;
    }

    .stage-label {
      padding: 0;
      text-align: left;
    }

    li:not(:last-child)::after {
      top: 33px;
      left: 13px;
      width: 2px;
      height: 6px;
    }
  }
</style>
