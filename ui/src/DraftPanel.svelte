<script>
  import TimeAgo from './TimeAgo.svelte';
  import ButtonHelp from './ButtonHelp.svelte';
  import Panel from './Panel.svelte';
  import ReviewFailure from './ReviewFailure.svelte';
  import { currentReviewRound } from './request-status.js';
  let { request, draft, action, onReject, onPublish, onClose } = $props();
  let confirmation = $state(null);
  let confirmationInvalid = $state(false);
  let decisionSubmitted = $state(false);
  const awaitingDecision = $derived(draft.decision === 'awaiting_decision' &&
    request.latest_round?.id === draft.round_id && request.latest_round?.status === 'awaiting_decision');
  const cleanupComplete = $derived(request.latest_round?.cleanup_pending === false &&
    request.latest_round?.cleanup_status === 'complete');
  const publishing = $derived(request.publication?.status === 'active');
  const changed = $derived(draft.head_sha !== request.head_sha || draft.revision !== request.revision ||
    draft.round_id !== request.latest_round?.id || draft.decision !== request.latest_round?.status);
  const fresh = $derived(draft.freshness === 'fresh' && !changed);
  const decisionReason = $derived(action?.pending ? 'Wait for the current action to finish.'
    : !cleanupComplete ? 'Sandbox cleanup must finish before deciding on this draft.'
    : confirmationInvalid ? 'Reopen the draft before making another decision.'
    : publishing ? 'Wait for the current publication to finish before making another decision.' : '');
  const publishReason = $derived(decisionReason || (!fresh
    ? draft.freshness === 'stale' || changed
      ? 'Publication is blocked because the PR changed or closed. You can reject this draft and prepare a new review.'
      : 'Publication is blocked until GitHub freshness is verified. Reopen the draft to check again.'
    : !draft.publisher_login ? 'Configure the publishing account and reopen the draft before publishing.' : ''));
  const publishDisabled = $derived(action?.pending || !cleanupComplete || confirmationInvalid || publishing || !fresh || !draft.publisher_login);
  const rejectDisabled = $derived(action?.pending || !cleanupComplete || confirmationInvalid || publishing);
  $effect(() => {
    if (confirmation && (!awaitingDecision || !cleanupComplete || publishing ||
      (confirmation.kind === 'publish' && (!fresh || !draft.publisher_login)))) confirmationInvalid = true;
  });

  function confirmDecision(kind) {
    if (!awaitingDecision || !cleanupComplete || publishing || action?.pending || decisionSubmitted || confirmationInvalid ||
      (kind === 'publish' && (!fresh || !draft.publisher_login))) return;
    confirmation = {
      kind, key: { ...draft.key }, round_id: draft.round_id, head_sha: draft.head_sha,
      body_hash: draft.publication_body_sha256, publisher_login: draft.publisher_login,
    };
  }

  async function decide() {
    if (!confirmation || confirmationInvalid || !awaitingDecision || !cleanupComplete || publishing || action?.pending || decisionSubmitted ||
      (confirmation.kind === 'publish' && (!fresh || !draft.publisher_login))) return;
    const result = await (confirmation.kind === 'publish' ? onPublish(confirmation) : onReject(confirmation));
    if (result) {
      decisionSubmitted = true;
      confirmation = null;
      confirmationInvalid = false;
      onClose();
    }
  }
  const freshnessLabels = { fresh: 'Fresh at inspection', stale: 'Stale', unknown: 'Unknown' };
  const verdictLabels = { ACCEPT: 'Accept', ACCEPT_WITH_NITS: 'Accept with nits', REQUEST_CHANGES: 'Request changes' };
  const decisionLabels = {
    awaiting_decision: 'Awaiting lead decision', publishing: 'Publishing', publication_unknown: 'Publication unknown',
    published: 'Published', rejected: 'Rejected',
  };
</script>

<Panel title="Draft review" label={`Private draft for ${request.repository} #${request.number}`}
  closeLabel="Close draft" closeDisabled={action?.pending} {onClose}>
  <p>Reviewed commit <a href={`https://github.com/${draft.key.repository}/commit/${draft.head_sha}`}
    title={draft.head_sha} target="_blank" rel="noopener noreferrer"><code>{draft.head_sha.slice(0, 7)}</code></a>
    · {draft.model} ({draft.agent})</p>
  {#if currentReviewRound(request)?.id === draft.round_id}
    <ReviewFailure {request} />
  {/if}
  {#if draft.freshness === 'stale'}
    <p class="warning" role="alert">The PR changed or closed. Publication is blocked; you can still reject this draft.</p>
  {:else if draft.freshness !== 'fresh'}
    <p class="warning" role="status">GitHub freshness could not be verified. Reopen the draft to check again before publishing. You can still reject it.</p>
  {/if}
  {#if changed}
    <p class="warning" role="status">The PR or review status changed. This view keeps the draft you opened. Reopen it to inspect the current state.</p>
  {/if}
  <p>Recommendation: <strong>{verdictLabels[draft.verdict] ?? 'Unknown'}</strong></p>
  <h4 class="panel-section-heading">Review comments</h4>
  <p>Publishing posts these comments to GitHub unchanged.</p>
  <div class="draft-text" role="region" aria-label="Review comments"><pre>{draft.publication_body}</pre></div>
  <h4 class="panel-section-heading">Private lead advice</h4>
  <p>This advice stays private when you publish.</p>
  <div class="draft-text" role="region" aria-label="Private lead advice"><pre>{draft.lead_advice || 'No private lead advice.'}</pre></div>
  <details class="panel-details">
    <summary>Review details</summary>
    <dl class="detail-list">
      <div><dt>Round</dt><dd><code>{draft.round_id}</code></dd></div>
      <div><dt>Reviewed commit</dt><dd><code>{draft.head_sha}</code></dd></div>
      <div><dt>Input revision</dt><dd>{draft.revision}</dd></div>
      <div><dt>Decision at inspection</dt><dd>{decisionLabels[draft.decision] ?? 'Unknown'}</dd></div>
      <div><dt>Freshness</dt><dd>{freshnessLabels[draft.freshness] ?? 'Unknown'}</dd></div>
      <div><dt>Publication body SHA-256</dt><dd><code>{draft.publication_body_sha256}</code></dd></div>
    </dl>
    <p>Freshness is checked against GitHub when this draft opens{#if draft.inspected_at} (<TimeAgo value={draft.inspected_at} />){/if}. Reopen it to check again.</p>
  </details>
  {#if awaitingDecision && !decisionSubmitted && !confirmation}
    {#if draft.publisher_login}
      <p>Publish as <strong>@{draft.publisher_login}</strong>.</p>
    {:else}
      <p class="warning">The publishing account is unavailable. Ask your assistant to configure it, then reopen this draft.</p>
    {/if}
  {/if}
  {#if confirmationInvalid && !action?.pending}
    <p class="action-error" role="alert">The pending decision is no longer current. Reopen the draft before deciding.</p>
  {/if}
  {#if confirmation}
    {@const publish = confirmation.kind === 'publish'}
    <Panel title={publish ? 'Publish this draft?' : 'Reject this draft?'}
      label={publish ? 'Confirm draft publication' : 'Confirm draft rejection'}
      headingLevel={4}
      closeLabel="Close confirmation" closeDisabled={action?.pending} onClose={() => confirmation = null}>
      {#if publish}
        <p>Post the review comments above unchanged to <a
          href={`https://github.com/${confirmation.key.repository}/pull/${confirmation.key.number}`}
          target="_blank" rel="noopener noreferrer">{confirmation.key.repository} #{confirmation.key.number}</a>
          at commit <a href={`https://github.com/${confirmation.key.repository}/commit/${confirmation.head_sha}`}
          title={confirmation.head_sha} target="_blank" rel="noopener noreferrer"><code>{confirmation.head_sha.slice(0, 7)}</code></a>
          as <strong>@{confirmation.publisher_login}</strong>.</p>
        <p>This posts a comment review without formally approving or merging the PR. Private lead advice stays private.</p>
      {:else}
        <p>Nothing will be posted. The private draft and current review permission are preserved.
          Another review still needs a confirmed model.</p>
      {/if}
      {#snippet actions()}
        {@const disabled = action?.pending || confirmationInvalid || !awaitingDecision || !cleanupComplete || publishing || (publish && (!fresh || !draft.publisher_login))}
        <ButtonHelp text={decisionReason || (publish ? publishReason || 'Recheck the PR and publishing account, then post these review comments unchanged.'
          : 'Reject this draft and clear its model confirmation. Any different, later model choice is kept.')}
          label={publish ? 'Confirm publication' : 'Confirm rejection'} {disabled}>
        {#snippet children(descriptionId)}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <md-filled-button {disabled} aria-describedby={descriptionId} onclick={decide}>
          {action?.pending ? (publish ? 'Starting publication…' : 'Rejecting…') : (publish ? 'Confirm publication' : 'Confirm rejection')}
        </md-filled-button>
        {/snippet}
        </ButtonHelp>
      {/snippet}
    </Panel>
  {/if}
  {#if draft.receipt}
    <p>Published as <a href={draft.receipt.url} target="_blank" rel="noopener noreferrer">GitHub review {draft.receipt.review_id}</a>
      by <strong>@{draft.receipt.publisher_login}</strong>.</p>
  {/if}
  {#snippet actions()}
    {#if awaitingDecision && !decisionSubmitted && !confirmation}
      <ButtonHelp text={decisionReason || 'Keep the draft private and decide not to publish it. You will confirm before rejecting.'}
        label="Reject draft" disabled={rejectDisabled}>
      {#snippet children(descriptionId)}
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <md-text-button disabled={rejectDisabled} aria-describedby={descriptionId}
        onclick={() => confirmDecision('reject')}>Reject draft</md-text-button>
      {/snippet}
      </ButtonHelp>
      <ButtonHelp text={publishReason || 'Post these review comments unchanged to GitHub. You will confirm the destination and publishing account.'}
        label="Publish draft" disabled={publishDisabled}>
      {#snippet children(descriptionId)}
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <md-filled-button disabled={publishDisabled} aria-describedby={descriptionId}
        onclick={() => confirmDecision('publish')}>Publish draft</md-filled-button>
      {/snippet}
      </ButtonHelp>
    {/if}
  {/snippet}
</Panel>
