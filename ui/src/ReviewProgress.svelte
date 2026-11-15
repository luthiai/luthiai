<script>
  import TimeAgo from './TimeAgo.svelte';
  import ReviewFailure from './ReviewFailure.svelte';
  import { currentReviewRound, reviewProgressSteps, reviewStartupFailed } from './request-status.js';
  let { request } = $props();
  const run = $derived(request.run);
  const active = $derived(run?.status === 'starting' || run?.status === 'running');
  const round = $derived(currentReviewRound(request));
  const receipt = $derived((round ?? (reviewStartupFailed(request) ? request.latest_round : null))?.receipt);
  const publication = $derived(request.publication?.round_id === round?.id ? request.publication : null);
  const reviewActive = $derived(active ||
    (['starting', 'running'].includes(round?.status) && round?.cleanup_status === 'in_progress'));
  const steps = $derived(reviewProgressSteps(request));
  const stepIcons = { current: '▶', done: '✓', pending: '○', attention: '!' };
  const stepLabels = { current: 'Current step', done: 'Completed', pending: 'Upcoming', attention: 'Needs attention' };
</script>

<ol class="review-steps" aria-label="Review steps" aria-live="polite">
  {#each steps as step (step.id)}
    <li data-state={step.state} aria-current={step.state === 'current' ? 'step' : undefined} aria-atomic="true">
      <span class="step-icon" aria-hidden="true">{stepIcons[step.state]}</span>
      <span><span class="visually-hidden">{stepLabels[step.state]}: </span>{step.label}</span>
    </li>
  {/each}
</ol>
<ReviewFailure {request} />
{#if publication?.status === 'active'}
  <p role="status">Verifying the publishing account and PR, then posting to GitHub…</p>
{:else if round?.status === 'publication_unknown' || round?.status === 'publishing'}
  <p class="warning" role="alert">Publication needs recovery. Check GitHub before deciding again; the GUI will not retry the post.</p>
  <p>Recover publication with:
    <code>luthiai recover-publication {request.repository} {request.number} --round {round.id}</code>
  </p>
{:else if round?.status === 'rejected'}
  <p>Draft rejected. Nothing was posted.</p>
{/if}

{#if receipt}
  <p>{round ? 'Published' : 'Previously published'} <TimeAgo value={receipt.published_at} /> as <a href={receipt.url} target="_blank" rel="noopener noreferrer">GitHub review {receipt.review_id}</a>
    by <strong>@{receipt.publisher_login}</strong>, tied to commit <a
      href={`https://github.com/${request.repository}/commit/${receipt.commit_id}`}
      title={receipt.commit_id} target="_blank" rel="noopener noreferrer"><code>{receipt.commit_id.slice(0, 7)}</code></a>.</p>
{/if}
{#if publication?.status === 'published' && publication.freshness === 'stale'}
  <p class="warning">PR inputs changed after publication. The review remains tied to its original commit.</p>
{:else if publication?.status === 'published' && publication.freshness === 'unknown'}
  <p class="warning">PR freshness could not be rechecked after publication. The saved receipt confirms publication.</p>
{/if}

{#if round?.status === 'awaiting_decision'}
  {#if round.freshness === 'stale'}
    <p class="warning">The PR changed or closed. Publication is blocked; you can still read and reject the draft.</p>
  {:else if round.freshness === 'unknown'}
    <p class="warning">PR freshness could not be verified. Open the draft to check again before publishing. You can still reject it.</p>
  {/if}
{/if}
{#if round?.cleanup_status === 'needs_attention'}
  <p class="warning">The review environment needs recovery.</p>
  <p>Recover the review environment with:
    <code>luthiai recover-review {request.repository} {request.number}</code>
  </p>
{:else if round?.cleanup_status === 'unknown'}
  <p class="warning">Cleanup status unavailable</p>
{:else if round?.cleanup_pending && !['starting', 'running'].includes(round.status)}
  <p role="status">Finishing cleanup…</p>
{/if}
{#if reviewActive || publication?.status === 'active'}
  <p class="note" role="note">Note: Closing this panel or browser tab does not stop the {publication?.status === 'active' ? 'publication' : 'review'}.
    {#if publication?.status !== 'active'}Stopping the <code>luthiai gui</code> command stops the running review.{/if}</p>
{/if}

<style>
  .review-steps {
    display: grid;
    gap: 12px;
    margin: 12px 0 20px;
    padding: 0;
    list-style: none;
    font-size: 0.88rem;
    line-height: 1.5;
  }

  .review-steps li {
    display: grid;
    grid-template-columns: 20px minmax(0, 1fr);
    gap: 8px;
    color: #5b5963;
  }

  .step-icon {
    text-align: center;
    font-weight: 700;
  }

  .review-steps li[data-state='current'] {
    color: var(--md-sys-color-primary);
    font-weight: 600;
  }

  .review-steps li[data-state='done'] .step-icon {
    color: #1d6b3c;
  }

  .review-steps li[data-state='attention'] {
    color: #a45e00;
    font-weight: 600;
  }
</style>
