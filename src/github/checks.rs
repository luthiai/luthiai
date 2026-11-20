//! Exact-head GitHub check runs and commit statuses for operator display.

use super::{GithubClient, GithubError, GithubResult, PullRequest};
use crate::config::CiCheckName;
use crate::preparation::{CiObservation, CiState};
use crate::{HeadSha, PrKey};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;

const PAGE_SIZE: usize = 100;
const MAX_RESULTS: usize = 1000;
const MAX_PAGES: usize = MAX_RESULTS / PAGE_SIZE;

pub struct CheckReport {
    head_sha: HeadSha,
    check_runs: Vec<CheckRun>,
    commit_statuses: Vec<CommitStatus>,
    combined_state: Option<CommitState>,
}

impl CheckReport {
    pub fn head_sha(&self) -> &HeadSha {
        &self.head_sha
    }

    pub fn preparation_observation(&self, ignored: &[CiCheckName]) -> CiObservation<'_> {
        let state = match self.summary_excluding(ignored) {
            CheckSummary::Green => CiState::Green,
            CheckSummary::Pending => CiState::Pending,
            CheckSummary::Failing => CiState::Failing,
            CheckSummary::Unknown => CiState::Unknown,
            CheckSummary::NoResults => CiState::NoResults,
        };
        CiObservation::new(&self.head_sha, state)
    }

    pub fn check_runs(&self) -> &[CheckRun] {
        &self.check_runs
    }

    pub fn commit_statuses(&self) -> &[CommitStatus] {
        &self.commit_statuses
    }

    pub fn summary(&self) -> CheckSummary {
        self.summary_excluding(&[])
    }

    /// Assess only checks whose exact names are not excluded by trusted configuration.
    pub fn summary_excluding(&self, ignored: &[CiCheckName]) -> CheckSummary {
        let is_ignored = |name: &str| ignored.iter().any(|check| check.as_str() == name);
        // GitHub's rollup includes ignored statuses. In that case, derive the
        // result from the remaining individual statuses instead.
        let combined_state = self.combined_state.filter(|_| {
            !self
                .commit_statuses
                .iter()
                .any(|status| is_ignored(&status.context))
        });
        let mut any = false;
        let mut pending = false;
        let mut unknown = false;
        for signal in self
            .check_runs
            .iter()
            .filter(|run| !is_ignored(&run.name))
            .map(CheckRun::signal)
            .chain(
                self.commit_statuses
                    .iter()
                    .filter(|status| !is_ignored(&status.context))
                    .map(CommitStatus::signal),
            )
            .chain(combined_state.map(CommitState::signal))
        {
            any = true;
            match signal {
                Signal::Failing => return CheckSummary::Failing,
                Signal::Unknown => unknown = true,
                Signal::Pending => pending = true,
                Signal::Passing => {}
            }
        }
        if !any {
            CheckSummary::NoResults
        } else if unknown {
            CheckSummary::Unknown
        } else if pending {
            CheckSummary::Pending
        } else {
            CheckSummary::Green
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckSummary {
    Green,
    Pending,
    Failing,
    Unknown,
    NoResults,
}

impl fmt::Display for CheckSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Green => "green",
            Self::Pending => "pending",
            Self::Failing => "failing",
            Self::Unknown => "unknown",
            Self::NoResults => "no checks reported",
        };
        f.write_str(label)
    }
}

#[derive(Deserialize)]
struct CheckRunsPage {
    total_count: u64,
    check_runs: Vec<CheckRun>,
}

#[derive(Deserialize)]
pub struct CheckRun {
    id: u64,
    pub name: String,
    head_sha: HeadSha,
    pub status: CheckRunStatus,
    pub conclusion: Option<CheckConclusion>,
}

impl CheckRun {
    fn signal(&self) -> Signal {
        match self.status {
            CheckRunStatus::Completed => match self.conclusion {
                Some(
                    CheckConclusion::Success | CheckConclusion::Neutral | CheckConclusion::Skipped,
                ) => Signal::Passing,
                Some(
                    CheckConclusion::Failure
                    | CheckConclusion::Cancelled
                    | CheckConclusion::TimedOut
                    | CheckConclusion::ActionRequired
                    | CheckConclusion::StartupFailure
                    | CheckConclusion::Stale,
                ) => Signal::Failing,
                Some(CheckConclusion::Unknown) | None => Signal::Unknown,
            },
            CheckRunStatus::Queued
            | CheckRunStatus::InProgress
            | CheckRunStatus::Waiting
            | CheckRunStatus::Requested
            | CheckRunStatus::Pending => Signal::Pending,
            CheckRunStatus::Unknown => Signal::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRunStatus {
    Queued,
    InProgress,
    Completed,
    Waiting,
    Requested,
    Pending,
    #[serde(other)]
    Unknown,
}

impl CheckRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::InProgress => "in progress",
            Self::Completed => "completed",
            Self::Waiting => "waiting",
            Self::Requested => "requested",
            Self::Pending => "pending",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckConclusion {
    Success,
    Failure,
    Neutral,
    Cancelled,
    Skipped,
    TimedOut,
    ActionRequired,
    StartupFailure,
    Stale,
    #[serde(other)]
    Unknown,
}

impl CheckConclusion {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Neutral => "neutral",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
            Self::TimedOut => "timed out",
            Self::ActionRequired => "action required",
            Self::StartupFailure => "startup failure",
            Self::Stale => "stale",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Deserialize)]
struct CombinedStatusPage {
    sha: HeadSha,
    state: CommitState,
    total_count: u64,
    statuses: Vec<CommitStatus>,
}

#[derive(Deserialize)]
pub struct CommitStatus {
    pub context: String,
    pub state: CommitState,
}

impl CommitStatus {
    fn signal(&self) -> Signal {
        self.state.signal()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitState {
    Success,
    Failure,
    Pending,
    Error,
    #[serde(other)]
    Unknown,
}

impl CommitState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Pending => "pending",
            Self::Error => "error",
            Self::Unknown => "unknown",
        }
    }

    fn signal(self) -> Signal {
        match self {
            Self::Success => Signal::Passing,
            Self::Failure | Self::Error => Signal::Failing,
            Self::Pending => Signal::Pending,
            Self::Unknown => Signal::Unknown,
        }
    }
}

enum Signal {
    Passing,
    Pending,
    Failing,
    Unknown,
}

impl GithubClient {
    pub fn checks_for_pull(&self, key: &PrKey, pull: &PullRequest) -> GithubResult<CheckReport> {
        let check_runs = self.check_runs_for_head(key, &pull.head_sha)?;
        let (commit_statuses, combined_state) =
            self.commit_statuses_for_head(key, &pull.head_sha)?;
        self.ensure_unchanged(key, pull, false)?;
        Ok(CheckReport {
            head_sha: pull.head_sha.clone(),
            check_runs,
            commit_statuses,
            combined_state,
        })
    }

    fn check_runs_for_head(&self, key: &PrKey, head: &HeadSha) -> GithubResult<Vec<CheckRun>> {
        let (runs, ids, pages) = self.collect_check_runs_for_head(key, head)?;
        if pages == 1 {
            return Ok(runs);
        }
        let (refreshed_runs, refreshed_ids, _) = self.collect_check_runs_for_head(key, head)?;
        if refreshed_ids != ids {
            return Err(GithubError::PaginationChanged {
                kind: "check runs",
                detail: "run IDs changed between page reads".to_owned(),
            });
        }
        Ok(refreshed_runs)
    }

    fn collect_check_runs_for_head(
        &self,
        key: &PrKey,
        head: &HeadSha,
    ) -> GithubResult<(Vec<CheckRun>, HashSet<u64>, usize)> {
        let mut runs = Vec::new();
        let mut seen_ids = HashSet::new();
        let mut expected_total = None;
        for page in 1..=MAX_PAGES {
            let data = self.check_runs_page(key, head, page)?;
            check_total(&mut expected_total, data.total_count, "check runs")?;
            let total_count = data.total_count;
            for run in data.check_runs {
                if !seen_ids.insert(run.id) {
                    return Err(GithubError::PaginationChanged {
                        kind: "check runs",
                        detail: format!("run {} appeared twice", run.id),
                    });
                }
                runs.push(run);
            }
            if runs.len() as u64 == total_count {
                return Ok((runs, seen_ids, page));
            }
            if runs.len() as u64 > total_count {
                return Err(GithubError::PaginationChanged {
                    kind: "check runs",
                    detail: "more runs than reported".to_owned(),
                });
            }
        }
        Err(GithubError::PaginationChanged {
            kind: "check runs",
            detail: "not every page was returned".to_owned(),
        })
    }

    fn check_runs_page(
        &self,
        key: &PrKey,
        head: &HeadSha,
        page: usize,
    ) -> GithubResult<CheckRunsPage> {
        let route = format!(
            "/repos/{}/commits/{head}/check-runs?per_page={PAGE_SIZE}&page={page}&filter=latest",
            key.repository()
        );
        let response = self.get(
            &route,
            "application/vnd.github+json",
            &format!("check runs for {key} at {head}"),
        )?;
        let data: CheckRunsPage =
            super::parse_json(response, format!("check runs for {key} at {head}"))?;
        for run in &data.check_runs {
            if &run.head_sha != head {
                return Err(GithubError::InvalidResponse {
                    resource: format!("check runs for {key} at {head}"),
                    detail: format!("run {} belongs to {}", run.id, run.head_sha),
                });
            }
        }
        Ok(data)
    }

    fn commit_statuses_for_head(
        &self,
        key: &PrKey,
        head: &HeadSha,
    ) -> GithubResult<(Vec<CommitStatus>, Option<CommitState>)> {
        let mut statuses = Vec::new();
        let mut expected_total = None;
        let mut combined_state = None;
        for page in 1..=MAX_PAGES {
            let route = format!(
                "/repos/{}/commits/{head}/status?per_page={PAGE_SIZE}&page={page}",
                key.repository()
            );
            let response = self.get(
                &route,
                "application/vnd.github+json",
                &format!("commit statuses for {key} at {head}"),
            )?;
            let data: CombinedStatusPage =
                super::parse_json(response, format!("commit statuses for {key} at {head}"))?;
            if &data.sha != head {
                return Err(GithubError::InvalidResponse {
                    resource: format!("commit statuses for {key} at {head}"),
                    detail: format!("response belongs to {}", data.sha),
                });
            }
            check_total(&mut expected_total, data.total_count, "commit statuses")?;
            if let Some(previous) = combined_state {
                if previous != data.state {
                    return Err(GithubError::PaginationChanged {
                        kind: "commit statuses",
                        detail: "combined state changed".to_owned(),
                    });
                }
            }
            combined_state = Some(data.state);
            statuses.extend(data.statuses);
            if statuses.len() as u64 == data.total_count {
                return Ok((
                    statuses,
                    if data.total_count == 0 {
                        None
                    } else {
                        combined_state
                    },
                ));
            }
            if statuses.len() as u64 > data.total_count {
                return Err(GithubError::PaginationChanged {
                    kind: "commit statuses",
                    detail: "more statuses than reported".to_owned(),
                });
            }
        }
        Err(GithubError::PaginationChanged {
            kind: "commit statuses",
            detail: "not every page was returned".to_owned(),
        })
    }
}

fn check_total(expected: &mut Option<u64>, actual: u64, what: &'static str) -> GithubResult<()> {
    if actual > MAX_RESULTS as u64 {
        return Err(GithubError::TooManyResults {
            kind: what,
            limit: MAX_RESULTS,
        });
    }
    if let Some(previous) = expected {
        if *previous != actual {
            return Err(GithubError::PaginationChanged {
                kind: what,
                detail: "total count changed".to_owned(),
            });
        }
    } else {
        *expected = Some(actual);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(status: &str, conclusion: Option<&str>) -> CheckRun {
        let value = serde_json::json!({
            "id": 1,
            "name": "job",
            "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "status": status,
            "conclusion": conclusion
        });
        serde_json::from_value(value).unwrap()
    }

    fn report(runs: Vec<CheckRun>, statuses: &[&str]) -> CheckReport {
        let commit_statuses = statuses
            .iter()
            .map(|state| {
                serde_json::from_value(serde_json::json!({"context":"ci", "state":state})).unwrap()
            })
            .collect();
        CheckReport {
            head_sha: HeadSha::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
            check_runs: runs,
            commit_statuses,
            combined_state: None,
        }
    }

    #[test]
    fn summary_distinguishes_green_pending_failing_unknown_and_no_results() {
        assert_eq!(report(vec![], &[]).summary(), CheckSummary::NoResults);
        assert_eq!(
            report(vec![run("completed", Some("success"))], &[]).summary(),
            CheckSummary::Green
        );
        assert_eq!(
            report(vec![run("completed", Some("neutral"))], &[]).summary(),
            CheckSummary::Green
        );
        assert_eq!(
            report(vec![run("completed", Some("skipped"))], &[]).summary(),
            CheckSummary::Green
        );
        assert_eq!(
            report(vec![run("in_progress", None)], &[]).summary(),
            CheckSummary::Pending
        );
        assert_eq!(
            report(vec![run("completed", Some("failure"))], &[]).summary(),
            CheckSummary::Failing
        );
        assert_eq!(report(vec![], &["error"]).summary(), CheckSummary::Failing);
        assert_eq!(
            report(vec![run("completed", None)], &[]).summary(),
            CheckSummary::Unknown
        );
    }

    #[test]
    fn empty_commit_statuses_do_not_make_successful_check_runs_pending() {
        let report = CheckReport {
            head_sha: HeadSha::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
            check_runs: vec![run("completed", Some("success"))],
            commit_statuses: vec![],
            combined_state: None,
        };
        assert_eq!(report.summary(), CheckSummary::Green);
    }

    #[test]
    fn ignored_check_runs_do_not_block_passing_checks() {
        let ignored = [CiCheckName::new("Optional Check").unwrap()];
        for (status, conclusion) in [
            ("in_progress", None),
            ("completed", Some("failure")),
            ("completed", None),
        ] {
            let mut optional_check = run(status, conclusion);
            optional_check.name = "Optional Check".to_owned();
            let report = report(vec![run("completed", Some("success")), optional_check], &[]);
            assert_ne!(report.summary(), CheckSummary::Green);
            assert_eq!(report.summary_excluding(&ignored), CheckSummary::Green);
            // Filtering for readiness must preserve the reported results for inspection.
            assert_eq!(report.check_runs().len(), 2);
        }
    }

    #[test]
    fn ignored_status_contexts_do_not_block_through_the_combined_rollup() {
        let ignored = [CiCheckName::new("Optional Check").unwrap()];
        for state in [
            CommitState::Failure,
            CommitState::Error,
            CommitState::Pending,
            CommitState::Unknown,
        ] {
            let mut report = report(vec![], &["success"]);
            report.commit_statuses.push(CommitStatus {
                context: "Optional Check".to_owned(),
                state,
            });
            report.combined_state = Some(state);
            assert_ne!(report.summary(), CheckSummary::Green);
            assert_eq!(report.summary_excluding(&ignored), CheckSummary::Green);
            assert_eq!(report.commit_statuses().len(), 2);
        }
    }

    #[test]
    fn exclusions_match_exact_names_and_keep_other_checks_blocking() {
        let ignored = [CiCheckName::new("Optional Check").unwrap()];
        for name in [
            "optional check",
            "Optional Check ",
            "Optional Check / other",
        ] {
            let mut report = report(vec![run("in_progress", None)], &[]);
            report.check_runs[0].name = name.to_owned();
            assert_eq!(report.summary_excluding(&ignored), CheckSummary::Pending);
            report.check_runs.clear();
            report.commit_statuses.push(CommitStatus {
                context: name.to_owned(),
                state: CommitState::Pending,
            });
            assert_eq!(report.summary_excluding(&ignored), CheckSummary::Pending);
        }

        for (status, conclusion, expected) in [
            ("queued", None, CheckSummary::Pending),
            ("completed", Some("failure"), CheckSummary::Failing),
            ("completed", None, CheckSummary::Unknown),
        ] {
            let mut optional_check = run("in_progress", None);
            optional_check.name = "Optional Check".to_owned();
            let report = report(vec![optional_check, run(status, conclusion)], &[]);
            assert_eq!(report.summary_excluding(&ignored), expected);
        }
        for (state, expected) in [
            ("pending", CheckSummary::Pending),
            ("failure", CheckSummary::Failing),
            ("unexpected", CheckSummary::Unknown),
        ] {
            let mut report = report(vec![], &[state]);
            report.commit_statuses.push(CommitStatus {
                context: "Optional Check".to_owned(),
                state: CommitState::Pending,
            });
            report.combined_state = Some(CommitState::Pending);
            assert_eq!(report.summary_excluding(&ignored), expected);
        }
    }

    #[test]
    fn ignoring_every_result_does_not_make_an_empty_result_set_green() {
        let ignored = [
            CiCheckName::new("job").unwrap(),
            CiCheckName::new("ci").unwrap(),
        ];
        let mut report = report(vec![run("completed", Some("success"))], &["success"]);
        report.combined_state = Some(CommitState::Success);
        assert_eq!(report.summary_excluding(&ignored), CheckSummary::NoResults);
        report.check_runs.clear();
        assert_eq!(report.summary_excluding(&ignored), CheckSummary::NoResults);
        report.commit_statuses.clear();
        report.combined_state = None;
        assert_eq!(report.summary_excluding(&ignored), CheckSummary::NoResults);
    }

    #[test]
    fn unmatched_exclusions_preserve_the_legacy_rollup() {
        let ignored = [CiCheckName::new("Optional Check").unwrap()];
        let mut report = report(vec![], &["success"]);
        report.combined_state = Some(CommitState::Failure);
        assert_eq!(report.summary_excluding(&ignored), CheckSummary::Failing);
    }
}
