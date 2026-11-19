import { act, render, screen, within } from '@testing-library/svelte';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import ReviewLifecycle from './ReviewLifecycle.svelte';
import { reviewSituation } from './request-status.js';

const now = Date.parse('2026-11-19T12:00:00Z') / 1000;
const snapshot = {
  head_sha: 'a'.repeat(40), revision: 2, authorization: 'awaiting_lead', discussion: {},
  context: { head_sha: 'a'.repeat(40), revision: 2, checked_at: now - 120 },
  latest_round: { id: 'r-current', status: 'awaiting_decision', model: 'Model.MixedCase', agent: 'codex',
    head_sha: 'a'.repeat(40), revision: 2, finished_at: now - 60,
    cleanup_pending: false, cleanup_status: 'complete' },
};

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(now * 1000);
});
afterEach(() => vi.useRealTimers());

it('shows completed actions in the past tense with recorded times, permission, and model details', async () => {
  render(ReviewLifecycle, { request: snapshot, situation: reviewSituation(snapshot) });
  const stage = (label: string) => screen.getByText(label).closest('li');
  const refresh = stage('PR refreshed');
  expect(within(refresh).getByText('2 minutes ago')).toBeTruthy();
  expect(refresh.querySelector('time').getAttribute('datetime')).toBe('2026-11-19T11:58:00.000Z');
  expect(stage('Review allowed').textContent).toContain('For this round');
  expect(stage('Review prepared').textContent).toContain('Model.MixedCase · codex');
  const review = stage('Review completed');
  expect(review.textContent).toContain('Private draft saved');
  expect(within(review).getByText('1 minute ago')).toBeTruthy();
  expect(review.querySelector('.stage-detail').textContent.trim()).toBe('Private draft saved · 1 minute ago');
  const decision = stage('Decide on draft');
  expect(decision.getAttribute('aria-current')).toBe('step');
  expect(decision.querySelector('.stage-detail')).toBeNull();

  await act(() => vi.advanceTimersByTime(60_000));
  expect(within(refresh).getByText('3 minutes ago')).toBeTruthy();
  expect(within(review).getByText('2 minutes ago')).toBeTruthy();
});

it('uses the publication receipt and clears old completed details when a new job starts', async () => {
  const published = { ...snapshot, latest_round: { ...snapshot.latest_round, status: 'published',
    receipt: { publisher_login: 'publisher', published_at: now - 30 } } };
  const { container, rerender } = render(ReviewLifecycle, { request: published, situation: reviewSituation(published) });
  const decision = screen.getByText('Draft published').closest('li');
  expect(decision.textContent).toContain('By @publisher');
  expect(within(decision).getByText('just now')).toBeTruthy();
  expect(decision.querySelector('.stage-detail').textContent.trim()).toBe('By @publisher · just now');
  expect(decision.querySelector('time').getAttribute('datetime')).toBe('2026-11-19T11:59:30.000Z');

  const starting = { ...published, run: { status: 'starting' } };
  await rerender({ request: starting, situation: reviewSituation(starting) });
  expect(screen.queryByText('Draft published')).toBeNull();
  expect(screen.queryByText('Model.MixedCase · codex')).toBeNull();
  expect(screen.getByText('Run review').closest('li').getAttribute('aria-current')).toBe('step');
  expect(screen.getByText('Decide on draft').closest('li').querySelector('.stage-detail')).toBeNull();

  const current = { ...snapshot, latest_round: { ...snapshot.latest_round, id: 'r-new', model: 'New.Model', finished_at: now } };
  await rerender({ request: current, situation: reviewSituation(current) });
  expect(screen.getByText('New.Model · codex')).toBeTruthy();
  expect(screen.getByText('Review completed').closest('li').querySelector('time').getAttribute('datetime'))
    .toBe('2026-11-19T12:00:00.000Z');
  expect(container.textContent).not.toContain('publisher');
});

it('omits unavailable timestamps rather than assigning a completion time in the browser', () => {
  const { checked_at, ...context } = snapshot.context;
  const { finished_at, ...latest_round } = snapshot.latest_round;
  const request = { ...snapshot, context, latest_round };
  const { container } = render(ReviewLifecycle, { request, situation: reviewSituation(request) });
  expect(screen.getByText('PR refreshed')).toBeTruthy();
  expect(screen.getByText('Review completed')).toBeTruthy();
  expect(container.querySelector('time')).toBeNull();
  expect(screen.queryByText('time unavailable')).toBeNull();
});

it('keeps unavailable stages as actions without completion details after closure', () => {
  const request = { ...snapshot, context: { ...snapshot.context, state: 'closed' },
    latest_round: { ...snapshot.latest_round, status: 'failed', cleanup_pending: true, cleanup_status: 'in_progress' } };
  const { container } = render(ReviewLifecycle, { request, situation: reviewSituation(request) });
  expect(screen.getByText('Refresh PR')).toBeTruthy();
  expect(screen.getByText('Allow review')).toBeTruthy();
  expect(container.querySelector('[aria-current]')).toBeNull();
  expect(container.querySelector('.stage-detail')).toBeNull();
});
