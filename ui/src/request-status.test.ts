import { describe, expect, it } from 'vitest';
import { reviewBlocker, reviewFailure, reviewLifecycleSteps, reviewProgressSteps, reviewSituation } from './request-status.js';

const request = {
  head_sha: 'a'.repeat(40), revision: 2, authorization: 'one_round', discussion: {},
  validated_model: { model: 'alpha', agent: 'runner' },
};
const preparation = {
  head_sha: request.head_sha, revision: 2, authorization: 'one_round', readiness: 'ready',
  validated_model: 'alpha', resolved_model: 'alpha', models: [{ model: 'alpha', agent: 'runner' }],
};

describe('review lifecycle', () => {
  it.each([
    { name: 'unobserved inputs', snapshot: { ...request, discussion: null }, states: ['current', 'pending', 'pending', 'pending', 'pending'] },
    { name: 'permission needed', snapshot: { ...request, authorization: 'awaiting_lead' }, states: ['done', 'current', 'pending', 'pending', 'pending'] },
    { name: 'preparation needed', snapshot: request, states: ['done', 'done', 'current', 'pending', 'pending'] },
    { name: 'CI blocked', snapshot: request, prepared: { ...preparation, readiness: 'checks_failing' }, states: ['done', 'done', 'attention', 'pending', 'pending'] },
    { name: 'ready to start', snapshot: request, prepared: preparation, states: ['done', 'done', 'done', 'current', 'pending'] },
    { name: 'consumed one-round permission', snapshot: { ...request, authorization: 'awaiting_lead', latest_round: { status: 'running' } },
      states: ['done', 'done', 'done', 'current', 'pending'] },
    { name: 'stale saved draft', snapshot: { ...request, latest_round: { status: 'awaiting_decision', freshness: 'stale' } },
      states: ['done', 'done', 'done', 'done', 'attention'] },
    { name: 'publication recovery', snapshot: { ...request, latest_round: { status: 'publication_unknown' } },
      states: ['done', 'done', 'done', 'done', 'attention'] },
    { name: 'confirmed publication', snapshot: { ...request, latest_round: { status: 'published', head_sha: request.head_sha, revision: 2 } },
      states: ['done', 'done', 'done', 'done', 'done'] },
  ])('shows established and upcoming stages for $name', ({ snapshot, prepared, states }) => {
    expect(reviewLifecycleSteps(reviewSituation(snapshot, prepared)).map((stage) => stage.state)).toEqual(states);
  });

  it.each(['published', 'rejected'])('starts the next lifecycle after a %s round when inputs change', (status) => {
    const snapshot = { ...request, authorization: 'awaiting_lead', revision: 3,
      latest_round: { status, head_sha: request.head_sha, revision: 2 } };
    expect(reviewLifecycleSteps(reviewSituation(snapshot)).map((stage) => stage.state))
      .toEqual(['done', 'current', 'pending', 'pending', 'pending']);
  });

  it('uses the new startup instead of an earlier completed round', () => {
    const snapshot = { ...request, latest_round: { status: 'published', head_sha: request.head_sha, revision: 2 },
      run: { status: 'starting' } };
    expect(reviewLifecycleSteps(reviewSituation(snapshot)).map((stage) => stage.state))
      .toEqual(['done', 'done', 'done', 'current', 'pending']);
    expect(reviewLifecycleSteps(reviewSituation({ ...snapshot, run: { status: 'failed', failure: 'stale_preparation' } }))
      .map((stage) => stage.state)).toEqual(['done', 'done', 'done', 'attention', 'pending']);
    expect(reviewLifecycleSteps(reviewSituation(snapshot, preparation, false, 'beta')).map((stage) => stage.state))
      .toEqual(['done', 'done', 'done', 'current', 'pending']);
  });
});

describe('review progress steps', () => {
  const round = { id: 'r-current', status: 'starting', cleanup_pending: true, cleanup_status: 'in_progress' };
  const running = { ...round, status: 'running', dispatched_at: 10 };
  const saved = { ...running, status: 'awaiting_decision' };
  const cleaned = { ...saved, cleanup_pending: false, cleanup_status: 'complete' };
  const run = { status: 'running', round_id: round.id, reviewer: 'claude' };

  it.each([
    { phase: 'startup before a round exists', snapshot: { run: { status: 'starting' } },
      states: ['current', 'pending', 'pending', 'pending'] },
    { phase: 'checkout and sandbox preparation', snapshot: { latest_round: round, run: { ...run, phase: 'preparing_checkout' } },
      states: ['current', 'pending', 'pending', 'pending'] },
    { phase: 'PR input and permission rechecks', snapshot: { latest_round: round, run: { ...run, phase: 'rechecking_inputs' } },
      states: ['done', 'current', 'pending', 'pending'] },
    { phase: 'reviewer execution', snapshot: { latest_round: running, run: { ...run, phase: 'running_reviewer' } },
      states: ['done', 'done', 'current', 'pending'] },
    { phase: 'private draft validation', snapshot: { latest_round: running, run: { ...run, phase: 'validating_draft' } },
      states: ['done', 'done', 'done', 'current'] },
    { phase: 'reviewing without an in-memory job', snapshot: { latest_round: running },
      states: ['done', 'done', 'current', 'pending'] },
    { phase: 'saved draft during cleanup', snapshot: { latest_round: saved, run: { ...run, phase: 'validating_draft' } },
      states: ['done', 'done', 'done', 'done'] },
    { phase: 'draft ready for the lead', snapshot: { latest_round: cleaned },
      states: ['done', 'done', 'done', 'done'] },
  ])('follows observed completion during $phase', ({ snapshot, states }) => {
    expect(reviewProgressSteps(snapshot).map((step) => step.state)).toEqual(states);
  });

  it.each([
    { phase: 'failed preparation', snapshot: { latest_round: { ...round, status: 'failed', failure: 'checkout',
      cleanup_pending: false, cleanup_status: 'complete' } }, states: ['attention', 'pending', 'pending', 'pending'] },
    { phase: 'failed rechecks', snapshot: { latest_round: { ...round, status: 'failed', failure: 'preflight' },
      run: { ...run, status: 'failed', phase: 'rechecking_inputs' } }, states: ['done', 'attention', 'pending', 'pending'] },
    { phase: 'failed review with cleanup still active', snapshot: { latest_round: { ...running, status: 'failed', failure: 'timeout' } },
      states: ['done', 'done', 'attention', 'pending'] },
    { phase: 'interrupted reviewer needing recovery', snapshot: { latest_round: { ...running, cleanup_status: 'needs_attention' } },
      states: ['done', 'done', 'attention', 'pending'] },
    { phase: 'invalid draft output', snapshot: { latest_round: { ...running, status: 'failed', failure: 'output' } },
      states: ['done', 'done', 'done', 'attention'] },
    { phase: 'failed cleanup after saving a draft', snapshot: { latest_round: { ...saved, cleanup_status: 'needs_attention' } },
      states: ['done', 'done', 'done', 'done'] },
    { phase: 'unknown cleanup', snapshot: { latest_round: { ...cleaned, cleanup_status: 'unknown' } },
      states: ['done', 'done', 'done', 'done'] },
  ])('leaves incomplete work unmarked during $phase', ({ snapshot, states }) => {
    expect(reviewProgressSteps(snapshot).map((step) => step.state)).toEqual(states);
  });

  it.each([
    { dispatched_at: undefined, phase: 'rechecking_inputs', states: ['done', 'attention', 'pending', 'pending'] },
    { dispatched_at: 10, phase: 'running_reviewer', states: ['done', 'done', 'attention', 'pending'] },
  ])('keeps dispatch failure progress consistent after restart with dispatched_at=$dispatched_at', ({ dispatched_at, phase, states }) => {
    const latest_round = { ...round, status: 'failed', failure: 'dispatch', dispatched_at,
      cleanup_pending: false, cleanup_status: 'complete' };
    expect(reviewProgressSteps({ latest_round, run: { ...run, status: 'failed', phase } }).map((step) => step.state))
      .toEqual(states);
    expect(reviewProgressSteps({ latest_round }).map((step) => step.state)).toEqual(states);
  });

  it.each(['publishing', 'publication_unknown', 'published', 'rejected'])('keeps the review complete while the later draft decision is %s', (status) => {
    const steps = reviewProgressSteps({ latest_round: { ...cleaned, status },
      publication: { round_id: round.id, status: 'unknown' } });
    expect(steps.map((step) => step.state)).toEqual(['done', 'done', 'done', 'done']);
    expect(steps.map((step) => step.id)).toEqual(['prepare', 'recheck', 'review', 'validate']);
  });

  it.each(['published', 'rejected'])('ignores an earlier %s round during a new startup and startup failure', (status) => {
    const latest_round = { ...cleaned, status };
    expect(reviewProgressSteps({ latest_round, run: { status: 'starting', round_id: null } }).map((step) => step.state))
      .toEqual(['current', 'pending', 'pending', 'pending']);
    expect(reviewProgressSteps({ latest_round, run: { status: 'failed', round_id: null } }).map((step) => step.state))
      .toEqual(['attention', 'pending', 'pending', 'pending']);
  });

  it.each([['claude', 'Claude'], ['codex', 'Codex']])('names the %s adapter only for its matching live job', (reviewer, name) => {
    expect(reviewProgressSteps({ latest_round: running, run: { ...run, reviewer } })[2].label).toBe(`Run ${name}`);
    expect(reviewProgressSteps({ latest_round: running, run: { ...run, reviewer, round_id: 'r-old', status: 'failed' } })[2].label)
      .toBe('Run reviewer');
  });
});

describe('review next steps', () => {
  it('keeps the running review primary after one-round permission is consumed', () => {
    const situation = reviewSituation({ ...request, authorization: 'awaiting_lead', latest_round: { status: 'running' } });
    expect(situation.title).toBe('Reviewing…');
    expect(situation.action).toBe('progress');
  });

  it('allows reading a saved draft while its runner finishes cleanup', () => {
    const situation = reviewSituation({ ...request, authorization: 'awaiting_lead',
      run: { status: 'running' }, latest_round: { status: 'awaiting_decision', cleanup_pending: true } });
    expect(situation.action).toBe('draft');
    expect(situation.message).toContain('when cleanup finishes');
  });

  it.each(['publishing', 'publication_unknown'])('requires recovery for a saved %s outcome without an active publisher', (status) => {
    const situation = reviewSituation({ ...request, latest_round: { status } });
    expect(situation.message).toContain('Check GitHub');
    expect(situation.action).toBe('progress');
  });

  it.each([
    { publication: { round_id: 'r-current', status: 'failed', error: 'Safe publication failure.' }, title: 'Publication failed' },
    { run: { round_id: 'r-current', status: 'failed', failure: 'execution' }, title: 'Draft saved with warning' },
  ])('puts $title ahead of a saved draft and cached preparation', ({ title, ...failure }) => {
    const failed = { ...request, ...failure, latest_round: { id: 'r-current', status: 'awaiting_decision' } };
    for (const cached of [undefined, preparation]) {
      const situation = reviewSituation(failed, cached);
      expect(situation.title).toBe(title);
      expect(situation.action).toBe('progress');
      expect(situation.label).toBe('View issue');
      expect(situation.tone).toBe('warning');
    }
  });

  it('keeps review failures visible until a fresh preparation replaces the failed attempt', () => {
    const failed = { ...request, run: { status: 'failed', failure: 'stale_preparation' } };
    expect(reviewSituation(failed, preparation, true).title).toBe('Review failed');
    expect(reviewSituation(failed, preparation).title).toBe('Ready');
  });

  it.each(['failed', 'interrupted'])('exposes a persisted %s round without a GUI job', (status) => {
    const failed = { ...request, latest_round: { id: 'r-current', status, failure: 'timeout' } };
    expect(reviewSituation(failed).title).toBe('Review failed');
    expect(reviewSituation(failed).label).toBe('View issue');
    expect(reviewFailure(failed).messages).toEqual([expect.stringContaining('Agent timed out')]);
  });

  it.each(['awaiting_decision', 'running', 'published', 'rejected'])('ignores failures from an older round while the current round is %s', (status) => {
    const current = { ...request, latest_round: { id: 'r-current', status },
      publication: { round_id: 'r-old', status: 'failed', error: 'Old publication error' },
      run: { round_id: 'r-old', status: 'failed', failure: 'execution' } };
    expect(reviewFailure(current)).toBeNull();
    expect(reviewSituation(current).label).not.toBe('View issue');
  });

  it.each(['published', 'rejected'])('clears warnings when a failed draft operation is resolved as %s', (status) => {
    expect(reviewFailure({ ...request, latest_round: { id: 'r-current', status },
      publication: { round_id: 'r-current', status: 'failed', error: 'Old publication error' },
      run: { round_id: 'r-current', status: 'failed', failure: 'execution' } })).toBeNull();
  });

  it('still exposes a later startup failure after an older draft was rejected', () => {
    const failed = { ...request, latest_round: { id: 'r-old', status: 'rejected' },
      run: { status: 'failed', failure: 'stale_preparation' } };
    expect(reviewSituation(failed).title).toBe('Review failed');
    expect(reviewFailure(failed).messages).toEqual([expect.stringContaining('Prepare and inspect the review again')]);
  });

  it('does not propose starting until fresh preparation and the selected model match saved validation', () => {
    expect(reviewSituation(request).action).toBe('prepare');
    expect(reviewSituation(request, preparation).action).toBe('run');
    expect(reviewSituation(request, preparation, false, 'beta').action).toBe('model');
    expect(reviewSituation({ ...request, revision: 3 }, preparation).action).toBe('prepare');
    expect(reviewSituation(request, preparation, true).action).toBe('prepare');
    expect(reviewSituation({ ...request, validated_model: null }, preparation).action).toBe('prepare');
    expect(reviewSituation({ ...request, validated_model: { model: 'alpha', agent: 'other' } }, preparation).action).toBe('prepare');
    expect(reviewSituation({ ...request, validated_model: { model: 'beta', agent: 'runner' } }, preparation).action).toBe('prepare');
  });

  it.each(['checks_pending', 'checks_failing', 'checks_unknown', 'no_checks'])('offers rechecking or inspection instead of starting for %s', (readiness) => {
    const situation = reviewSituation(request, { ...preparation, readiness });
    expect(situation.action).toBe(readiness === 'checks_pending' ? 'prepare' : 'checks');
    expect(situation.secondaryAction).toBe(readiness === 'checks_pending' ? 'checks' : null);
  });

  it('does not mistake an earlier publication for a review of newer discussion', () => {
    const latest_round = { status: 'published', head_sha: request.head_sha, revision: 1 };
    expect(reviewSituation({ ...request, latest_round }).action).toBe('prepare');
    expect(reviewSituation({ ...request, latest_round: { ...latest_round, revision: 2 } }).action).toBe('receipt');
  });

  it('names the actual blocker for a draft decision or unfinished cleanup', () => {
    expect(reviewBlocker({ ...request, latest_round: { status: 'awaiting_decision' } })).toContain('Publish or reject');
    expect(reviewBlocker({ ...request, latest_round: { status: 'failed', cleanup_pending: true, cleanup_status: 'needs_attention' } })).toContain('Recover');
    expect(reviewBlocker(request)).toBe('');
  });
});
